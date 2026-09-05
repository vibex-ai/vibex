//! Source-specific import of local Agent session history.
//!
//! The picker performs a lightweight metadata pass for every known local
//! layout.  It never walks arbitrary files below an Agent home, and it keeps a
//! short-lived locator for each listed session so normal import selection opens
//! the exact transcript or database row that was already discovered.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Map, Value};
use vibex_core::{
    AgentMessagePayload, AgentSession, AgentSessionSafety, AgentSessionState,
    LocalHistoryImportRecord, LocalHistoryImportStatus, LocalHistoryKey,
    LocalHistoryMaterializedSession, LocalHistoryScanDiagnostic, LocalHistoryScanFolder,
    LocalHistoryScanResult, LocalHistoryScanSession, LocalHistorySelection,
    LocalHistorySessionSummary, LocalHistorySource, LocalHistoryTimelineEntry, MessageAttachment,
    ReasoningPayload, SystemNoticeLevel, SystemNoticePayload, TimelinePayload, TimelineSource,
    ToolCallPayload, ToolCallStatus, UserMessagePayload, VibexError, VibexResult,
};

const MAX_SESSIONS_PER_SOURCE: usize = 10_000;
const MAX_DIAGNOSTICS: usize = 64;
const MAX_TITLE_CHARS: usize = 240;
const MAX_TEXT_CHARS: usize = 100_000;
const MAX_TOOL_SUMMARY_CHARS: usize = 4_000;

#[derive(Debug, Clone)]
pub struct LocalHistorySourceRoot {
    pub source: LocalHistorySource,
    /// Agent-owned state home. Each scanner chooses its own documented child
    /// directories from this root rather than treating it as a generic tree.
    pub root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    modified: Option<SystemTime>,
    size: u64,
}

#[derive(Debug, Clone)]
struct CachedSummary {
    fingerprint: Fingerprint,
    summary: LocalHistorySessionSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SummaryCacheKey {
    source: LocalHistorySource,
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LocatorKey {
    source: LocalHistorySource,
    root: PathBuf,
    external_id: String,
}

#[derive(Debug, Clone)]
enum LocalHistoryLocator {
    Transcript(PathBuf),
    Cline {
        data_root: PathBuf,
        task_id: String,
    },
    OpenCode {
        database: PathBuf,
        session_id: String,
    },
    Zcode {
        database: PathBuf,
        session_id: String,
    },
    Hermes {
        database: PathBuf,
        session_id: String,
    },
    DeepSeek {
        session_dir: PathBuf,
        attachments_root: PathBuf,
    },
    Cursor {
        database: PathBuf,
        session_id: String,
    },
    Antigravity {
        database: PathBuf,
        metadata: Option<PathBuf>,
    },
}

#[derive(Debug, Clone)]
struct FoundSession {
    summary: LocalHistorySessionSummary,
    locator: LocalHistoryLocator,
}

#[derive(Debug, Default)]
struct ScanBatch {
    found: Vec<FoundSession>,
    diagnostics: Vec<LocalHistoryScanDiagnostic>,
}

fn summary_cache() -> &'static Mutex<HashMap<SummaryCacheKey, CachedSummary>> {
    static CACHE: OnceLock<Mutex<HashMap<SummaryCacheKey, CachedSummary>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn locator_cache() -> &'static Mutex<HashMap<LocatorKey, LocalHistoryLocator>> {
    static CACHE: OnceLock<Mutex<HashMap<LocatorKey, LocalHistoryLocator>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let metadata = fs::metadata(path).ok()?;
    Some(Fingerprint {
        modified: metadata.modified().ok(),
        size: metadata.len(),
    })
}

fn cacheable(source: LocalHistorySource) -> bool {
    matches!(
        source,
        LocalHistorySource::Claude
            | LocalHistorySource::Codex
            | LocalHistorySource::CodeBuddy
            | LocalHistorySource::Pi
    )
}

fn cached_file_summary(
    source: LocalHistorySource,
    path: &Path,
    parse: impl FnOnce() -> Result<Option<LocalHistorySessionSummary>, String>,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    if !cacheable(source) {
        return parse();
    }
    let key = SummaryCacheKey {
        source,
        path: path.to_path_buf(),
    };
    let before = fingerprint(path);
    if let Some(before) = before {
        if let Ok(cache) = summary_cache().lock() {
            if let Some(cached) = cache
                .get(&key)
                .filter(|cached| cached.fingerprint == before)
            {
                return Ok(Some(cached.summary.clone()));
            }
        }
    }
    let parsed = parse()?;
    if let (Some(before), Some(summary)) = (before, parsed.as_ref()) {
        if fingerprint(path) == Some(before) {
            if let Ok(mut cache) = summary_cache().lock() {
                cache.insert(
                    key,
                    CachedSummary {
                        fingerprint: before,
                        summary: summary.clone(),
                    },
                );
            }
        }
    }
    Ok(parsed)
}

fn source_root(source: LocalHistorySource) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let env_path = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let xdg_data = || std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty());
    let xdg_config = || std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty());

    Some(match source {
        LocalHistorySource::Claude => {
            env_path("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude"))
        }
        LocalHistorySource::Codex => env_path("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
        LocalHistorySource::OpenCode => env_path("OPENCODE_DATA_DIR")
            .or_else(|| xdg_data().map(|path| PathBuf::from(path).join("opencode")))
            .unwrap_or_else(|| home.join(".local").join("share").join("opencode")),
        LocalHistorySource::Gemini => {
            resolve_gemini_base_dir_from(std::env::var_os("GEMINI_CLI_HOME"), Some(home.clone()))
        }
        LocalHistorySource::Cline => {
            env_path("CLINE_DIR").unwrap_or_else(|| home.join(".cline").join("data"))
        }
        LocalHistorySource::Hermes => {
            env_path("HERMES_HOME").unwrap_or_else(|| home.join(".hermes"))
        }
        LocalHistorySource::CodeBuddy => {
            env_path("CODEBUDDY_CONFIG_DIR").unwrap_or_else(|| home.join(".codebuddy"))
        }
        LocalHistorySource::Kimi => {
            env_path("KIMI_CODE_HOME").unwrap_or_else(|| home.join(".kimi-code"))
        }
        LocalHistorySource::Pi => env_path("PI_CODING_AGENT_SESSION_DIR")
            .or_else(|| env_path("PI_CODING_AGENT_DIR").map(|path| path.join("sessions")))
            .unwrap_or_else(|| home.join(".pi").join("agent").join("sessions")),
        LocalHistorySource::Grok => env_path("GROK_HOME").unwrap_or_else(|| home.join(".grok")),
        LocalHistorySource::Cursor => env_path("CURSOR_CONFIG_DIR")
            .or_else(|| xdg_config().map(|path| PathBuf::from(path).join("cursor")))
            .unwrap_or_else(|| home.join(".cursor")),
        LocalHistorySource::DeepSeek => resolve_deepseek_sessions_root_from(
            std::env::var_os("DEEPSEEK_ACP_SESSIONS_ROOT"),
            std::env::var_os("DSH_HOME"),
            Some(home.clone()),
        ),
        LocalHistorySource::Zcode => env_path("ZCODE_HOME").unwrap_or_else(|| home.join(".zcode")),
        LocalHistorySource::Antigravity => {
            resolve_antigravity_acp_dir_from(std::env::var_os("GEMINI_HOME"), Some(home))
        }
    })
}

/// Gemini CLI treats its override as a parent directory and appends `.gemini`.
fn resolve_gemini_base_dir_from(
    gemini_cli_home_env: Option<OsString>,
    home_dir: Option<PathBuf>,
) -> PathBuf {
    gemini_cli_home_env
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or(home_dir)
        .unwrap_or_default()
        .join(".gemini")
}

/// Expand only the `~` and `~/...` forms used by the local Agent stores.
/// `~user/...` is intentionally left untouched because it is not expanded by
/// the source clients either.
fn expand_home_prefix(value: &str, home_dir: Option<&Path>) -> PathBuf {
    if value == "~" {
        if let Some(home) = home_dir {
            return home.to_path_buf();
        }
    } else if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = home_dir {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

fn resolve_dsh_home_from(dsh_home_env: Option<OsString>, home_dir: Option<PathBuf>) -> PathBuf {
    if let Some(value) = dsh_home_env.filter(|value| !value.to_string_lossy().trim().is_empty()) {
        return expand_home_prefix(&value.to_string_lossy(), home_dir.as_deref());
    }
    home_dir.unwrap_or_default().join(".dsh")
}

fn resolve_deepseek_sessions_root_from(
    sessions_env: Option<OsString>,
    dsh_home_env: Option<OsString>,
    home_dir: Option<PathBuf>,
) -> PathBuf {
    sessions_env
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| resolve_dsh_home_from(dsh_home_env, home_dir).join("sessions"))
}

fn resolve_antigravity_acp_dir_from(
    gemini_home_env: Option<OsString>,
    home_dir: Option<PathBuf>,
) -> PathBuf {
    let gemini_home = gemini_home_env
        .filter(|value| !value.is_empty())
        .map(|value| expand_home_prefix(&value.to_string_lossy(), home_dir.as_deref()))
        .or_else(|| home_dir.map(|home| home.join(".gemini")))
        .unwrap_or_default();
    gemini_home.join("antigravity-acp")
}

/// Return every known local store root. Missing paths are retained because a
/// scan of an unavailable source is a cheap no-op and keeps the registry stable.
pub fn local_history_source_roots() -> Vec<LocalHistorySourceRoot> {
    LocalHistorySource::ALL
        .into_iter()
        .filter_map(|source| {
            source_root(source).map(|root| LocalHistorySourceRoot { source, root })
        })
        .collect()
}

pub fn scan_local_history() -> LocalHistoryScanResult {
    scan_local_history_from(&local_history_source_roots(), &[])
}

pub fn scan_local_history_from(
    roots: &[LocalHistorySourceRoot],
    imported: &[LocalHistoryImportRecord],
) -> LocalHistoryScanResult {
    let imported = imported
        .iter()
        .cloned()
        .map(|record| (record.key.clone(), record))
        .collect::<HashMap<_, _>>();
    let mut workers = Vec::with_capacity(roots.len());
    for root in roots.iter().cloned() {
        workers.push((root.source, std::thread::spawn(move || scan_source(root))));
    }

    let mut found = Vec::new();
    let mut diagnostics = Vec::new();
    for (source, worker) in workers {
        match worker.join() {
            Ok(mut batch) => {
                found.append(&mut batch.found);
                diagnostics.append(&mut batch.diagnostics);
            }
            Err(_) => diagnostics.push(diagnostic(
                source,
                "local_history_scan_worker_failed",
                "A local history scanner stopped unexpectedly",
            )),
        }
    }

    // Keep the first source-specific location for each stable external id.
    let mut seen = HashSet::new();
    found.retain(|item| seen.insert(item.summary.key.clone()));
    found.sort_by(|left, right| summary_sort(&left.summary, &right.summary));

    let mut folders: BTreeMap<String, LocalHistoryScanFolder> = BTreeMap::new();
    let mut unassigned_count = 0u32;
    for item in found {
        let key = item.summary.key.clone();
        let status = imported
            .get(&key)
            .map(|record| {
                if record.deleted {
                    LocalHistoryImportStatus::Deleted
                } else {
                    LocalHistoryImportStatus::Imported
                }
            })
            .unwrap_or(LocalHistoryImportStatus::New);
        let Some(workspace_root) = item
            .summary
            .workspace_root
            .as_deref()
            .map(normalize_workspace_root)
            .filter(|path| !path.is_empty())
        else {
            unassigned_count = unassigned_count.saturating_add(1);
            continue;
        };
        let folder =
            folders
                .entry(workspace_root.clone())
                .or_insert_with(|| LocalHistoryScanFolder {
                    workspace_root,
                    sources: Vec::new(),
                    sessions: Vec::new(),
                });
        if !folder.sources.contains(&key.source) {
            folder.sources.push(key.source);
            folder.sources.sort_by_key(|source| source.key());
        }
        folder.sessions.push(LocalHistoryScanSession {
            summary: item.summary,
            status,
        });
    }

    let mut folders = folders.into_values().collect::<Vec<_>>();
    for folder in &mut folders {
        folder
            .sessions
            .sort_by(|left, right| summary_sort(&left.summary, &right.summary));
    }
    let listed_count = folders
        .iter()
        .map(|folder| folder.sessions.len() as u32)
        .sum::<u32>();
    let importable_count = folders
        .iter()
        .flat_map(|folder| &folder.sessions)
        .filter(|session| session.status == LocalHistoryImportStatus::New)
        .count() as u32;
    diagnostics.truncate(MAX_DIAGNOSTICS);
    LocalHistoryScanResult {
        folders,
        total_sessions: listed_count.saturating_add(unassigned_count),
        importable_count,
        unassigned_count,
        diagnostics,
    }
}

fn scan_source(root: LocalHistorySourceRoot) -> ScanBatch {
    let batch = match root.source {
        LocalHistorySource::Claude => scan_claude(&root),
        LocalHistorySource::Codex => scan_codex(&root),
        LocalHistorySource::Gemini => scan_gemini(&root),
        LocalHistorySource::Cline => scan_cline(&root),
        LocalHistorySource::OpenCode => scan_opencode(&root),
        LocalHistorySource::Zcode => scan_zcode(&root),
        LocalHistorySource::Hermes => scan_hermes(&root),
        LocalHistorySource::CodeBuddy => scan_codebuddy(&root),
        LocalHistorySource::Kimi => scan_kimi(&root),
        LocalHistorySource::Pi => scan_pi(&root),
        LocalHistorySource::Grok => scan_grok(&root),
        LocalHistorySource::Cursor => scan_cursor(&root),
        LocalHistorySource::DeepSeek => scan_deepseek(&root),
        LocalHistorySource::Antigravity => scan_antigravity(&root),
    };
    if let Ok(mut cache) = locator_cache().lock() {
        for item in &batch.found {
            cache.insert(
                LocatorKey {
                    source: root.source,
                    root: root.root.clone(),
                    external_id: item.summary.key.external_id.clone(),
                },
                item.locator.clone(),
            );
        }
    }
    batch
}

fn summary_sort(
    left: &LocalHistorySessionSummary,
    right: &LocalHistorySessionSummary,
) -> std::cmp::Ordering {
    right
        .updated_at_ms
        .or(right.started_at_ms)
        .cmp(&left.updated_at_ms.or(left.started_at_ms))
        .then_with(|| left.key.source.key().cmp(right.key.source.key()))
        .then_with(|| left.key.external_id.cmp(&right.key.external_id))
}

fn diagnostic(source: LocalHistorySource, code: &str, message: &str) -> LocalHistoryScanDiagnostic {
    LocalHistoryScanDiagnostic {
        source,
        code: code.to_string(),
        message: bounded_text(message, 180),
    }
}

fn append_summary(
    batch: &mut ScanBatch,
    source: LocalHistorySource,
    parsed: Result<Option<LocalHistorySessionSummary>, String>,
    locator: LocalHistoryLocator,
) {
    match parsed {
        Ok(Some(summary)) => batch.found.push(FoundSession { summary, locator }),
        Ok(None) => {}
        Err(_) => batch.diagnostics.push(diagnostic(
            source,
            "local_history_file_unreadable",
            "A local history file could not be read",
        )),
    }
}

fn root_child(root: &Path, child: &str) -> PathBuf {
    if root.file_name().and_then(|name| name.to_str()) == Some(child) {
        root.to_path_buf()
    } else {
        root.join(child)
    }
}

fn direct_subdirs(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn direct_files(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
}

fn for_each_jsonl(path: &Path, mut visit: impl FnMut(Value)) -> Result<(), String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|error| error.to_string())?;
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            visit(value);
        }
    }
    Ok(())
}

fn json_file(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn timestamp_value(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(if number.unsigned_abs() < 100_000_000_000 {
            number.saturating_mul(1000)
        } else {
            number
        });
    }
    if let Some(number) = value.as_f64() {
        let millis = if number.abs() < 100_000_000_000.0 {
            number * 1000.0
        } else {
            number
        };
        return (millis.is_finite() && millis >= i64::MIN as f64 && millis <= i64::MAX as f64)
            .then_some(millis as i64);
    }
    let text = value.as_str()?.trim();
    if let Ok(number) = text.parse::<i64>() {
        return timestamp_value(&Value::Number(number.into()));
    }
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

fn field_timestamp(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(timestamp_value)
}

fn set_first_string(slot: &mut Option<String>, candidate: Option<String>) {
    if slot.is_none() {
        *slot = candidate;
    }
}

fn set_last_string(slot: &mut Option<String>, candidate: Option<String>) {
    if candidate.is_some() {
        *slot = candidate;
    }
}

fn set_first_i64(slot: &mut Option<i64>, candidate: Option<i64>) {
    if slot.is_none() {
        *slot = candidate;
    }
}

fn set_last_i64(slot: &mut Option<i64>, candidate: Option<i64>) {
    if candidate.is_some() {
        *slot = candidate;
    }
}

fn title_from_text(text: &str) -> String {
    bounded_text(
        text.lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(text),
        MAX_TITLE_CHARS,
    )
}

fn bounded_text(value: &str, max: usize) -> String {
    value.trim().chars().take(max).collect()
}

fn normalize_workspace_root(value: &str) -> String {
    let mut normalized = value.trim().replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn build_summary(
    source: LocalHistorySource,
    external_id: impl Into<String>,
    title: Option<String>,
    workspace_root: Option<String>,
    source_path: &Path,
    started_at_ms: Option<i64>,
    updated_at_ms: Option<i64>,
    message_count: u32,
    model: Option<String>,
) -> Option<LocalHistorySessionSummary> {
    let external_id = external_id.into();
    if external_id.trim().is_empty() || message_count == 0 {
        return None;
    }
    Some(LocalHistorySessionSummary {
        key: LocalHistoryKey {
            source,
            external_id,
        },
        agent_id: source.agent_id(),
        title: title
            .filter(|title| !title.trim().is_empty())
            .map(|title| bounded_text(&title, MAX_TITLE_CHARS))
            .unwrap_or_else(|| format!("{} session", source.label())),
        workspace_root: workspace_root
            .map(|path| normalize_workspace_root(&path))
            .filter(|path| !path.is_empty()),
        source_path: path_string(source_path),
        started_at_ms,
        updated_at_ms,
        message_count,
        model: model
            .map(|model| bounded_text(&model, MAX_TITLE_CHARS))
            .filter(|model| !model.is_empty()),
    })
}

fn user_entry(text: String, timestamp_ms: Option<i64>) -> Option<LocalHistoryTimelineEntry> {
    user_entry_with_attachments(text, Vec::new(), timestamp_ms)
}

fn user_entry_with_attachments(
    text: String,
    attachments: Vec<MessageAttachment>,
    timestamp_ms: Option<i64>,
) -> Option<LocalHistoryTimelineEntry> {
    let text = bounded_text(&text, MAX_TEXT_CHARS);
    (!text.is_empty() || !attachments.is_empty()).then(|| LocalHistoryTimelineEntry {
        source: TimelineSource::User,
        payload: TimelinePayload::UserMessage(UserMessagePayload { text, attachments }),
        timestamp_ms,
    })
}

fn agent_entry(text: String, timestamp_ms: Option<i64>) -> Option<LocalHistoryTimelineEntry> {
    let text = bounded_text(&text, MAX_TEXT_CHARS);
    (!text.is_empty()).then(|| LocalHistoryTimelineEntry {
        source: TimelineSource::Agent,
        payload: TimelinePayload::AgentMessage(AgentMessagePayload {
            text,
            is_final: true,
        }),
        timestamp_ms,
    })
}

fn reasoning_entry(text: String, timestamp_ms: Option<i64>) -> Option<LocalHistoryTimelineEntry> {
    let text = bounded_text(&text, MAX_TEXT_CHARS);
    (!text.is_empty()).then(|| LocalHistoryTimelineEntry {
        source: TimelineSource::Agent,
        payload: TimelinePayload::Reasoning(ReasoningPayload {
            text,
            is_final: true,
        }),
        timestamp_ms,
    })
}

fn tool_entry(
    id: Option<String>,
    name: Option<String>,
    input: Option<String>,
    output: Option<String>,
    failed: bool,
    timestamp_ms: Option<i64>,
) -> LocalHistoryTimelineEntry {
    LocalHistoryTimelineEntry {
        source: TimelineSource::Agent,
        payload: TimelinePayload::ToolCall(ToolCallPayload {
            tool_call_id: id.unwrap_or_else(|| "local-tool".to_string()),
            tool_name: name.unwrap_or_else(|| "tool".to_string()),
            status: if failed {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            },
            summary: if failed {
                "Imported tool failure".to_string()
            } else {
                "Imported tool call".to_string()
            },
            input_summary: input
                .map(|value| bounded_text(&value, MAX_TOOL_SUMMARY_CHARS))
                .filter(|value| !value.is_empty()),
            output_summary: output
                .map(|value| bounded_text(&value, MAX_TOOL_SUMMARY_CHARS))
                .filter(|value| !value.is_empty()),
            raw_extension: None,
        }),
        timestamp_ms,
    }
}

fn require_timeline(
    timeline: Vec<LocalHistoryTimelineEntry>,
) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    if timeline.is_empty() {
        Err(VibexError::validation(
            "local_history_timeline_empty",
            "selected local history has no readable messages",
        ))
    } else {
        Ok(timeline)
    }
}

fn text_string_or_parts(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.as_str()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToOwned::to_owned)
                    .or_else(|| string_field(part, "text"))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn json_preview(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    if let Some(text) = value.as_str() {
        return (!text.trim().is_empty()).then(|| text.trim().to_string());
    }
    serde_json::to_string(value).ok()
}

const CLAUDE_SYSTEM_TAGS: [&str; 8] = [
    "system-reminder",
    "local-command-caveat",
    "command-name",
    "command-message",
    "command-args",
    "local-command-stdout",
    "user-prompt-submit-hook",
    "fast_mode_info",
];
const CLAUDE_CONTEXT_CONTINUATION_PREFIX: &str =
    "This session is being continued from a previous conversation";

/// Remove provider-injected XML blocks without relying on a regular-expression
/// engine. A malformed or unterminated tag is left visible so a truncated
/// transcript does not silently lose user text.
fn strip_claude_system_tags(text: &str) -> Option<String> {
    let mut output = text.to_string();
    for tag in CLAUDE_SYSTEM_TAGS {
        loop {
            let Some(start) = output.find(&format!("<{tag}")) else {
                break;
            };
            let Some(open_end) = output[start..].find('>') else {
                break;
            };
            let close = format!("</{tag}>");
            let body_start = start + open_end + 1;
            let Some(close_offset) = output[body_start..].find(&close) else {
                break;
            };
            let end = body_start + close_offset + close.len();
            output.replace_range(start..end, "");
        }
    }
    let output = output.trim();
    (!output.is_empty()).then(|| output.to_string())
}

fn claude_tag_value(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    let value = text[start..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn claude_slash_command_display(text: &str) -> Option<String> {
    let name = claude_tag_value(text, "command-name")?;
    if !name.starts_with('/') {
        return None;
    }
    let args = claude_tag_value(text, "command-args");
    Some(match args {
        Some(args) => format!("{name} {args}"),
        None => name,
    })
}

fn claude_message_content(value: &Value) -> Option<&Value> {
    value.pointer("/message/content")
}

fn claude_user_text(value: &Value) -> String {
    let Some(content) = claude_message_content(value) else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return claude_slash_command_display(text)
            .or_else(|| strip_claude_system_tags(text))
            .unwrap_or_default();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .filter_map(strip_claude_system_tags)
        .collect::<Vec<_>>()
        .join("\n")
}

fn claude_assistant_text(value: &Value) -> String {
    let Some(content) = claude_message_content(value) else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.trim().to_string();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn claude_context_continuation(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("user")
        && claude_user_text(value).starts_with(CLAUDE_CONTEXT_CONTINUATION_PREFIX)
}

fn claude_tool_result_only(value: &Value) -> bool {
    claude_message_content(value)
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            !parts.is_empty()
                && parts.iter().all(|part| {
                    matches!(
                        part.get("type").and_then(Value::as_str),
                        Some("tool_result" | "server_tool_result")
                    )
                })
        })
}

fn claude_interrupt_marker(value: &Value) -> bool {
    if value.get("type").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let Some(content) = claude_message_content(value) else {
        return false;
    };
    let text = match content {
        Value::String(text) => Some(text.as_str()),
        Value::Array(parts) if parts.len() == 1 => parts[0]
            .get("type")
            .and_then(Value::as_str)
            .filter(|kind| *kind == "text")
            .and_then(|_| parts[0].get("text"))
            .and_then(Value::as_str),
        _ => None,
    };
    matches!(
        text,
        Some("[Request interrupted by user]" | "[Request interrupted by user for tool use]")
    )
}

fn system_entry(text: String, timestamp_ms: Option<i64>) -> Option<LocalHistoryTimelineEntry> {
    let text = bounded_text(&text, MAX_TEXT_CHARS);
    (!text.is_empty()).then(|| LocalHistoryTimelineEntry {
        source: TimelineSource::System,
        payload: TimelinePayload::SystemNotice(SystemNoticePayload {
            level: SystemNoticeLevel::Info,
            message: text,
        }),
        timestamp_ms,
    })
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

fn scan_claude(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let projects = root_child(&root.root, "projects");
    for project in direct_subdirs(&projects) {
        let fallback_workspace = project
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.replace('-', "/"));
        for path in direct_files(&project)
            .into_iter()
            .filter(|path| is_jsonl(path))
        {
            let parsed = cached_file_summary(LocalHistorySource::Claude, &path, || {
                parse_claude_summary(&path, fallback_workspace.clone())
            });
            append_summary(
                &mut batch,
                LocalHistorySource::Claude,
                parsed,
                LocalHistoryLocator::Transcript(path),
            );
            if batch.found.len() >= MAX_SESSIONS_PER_SOURCE {
                return batch;
            }
        }
    }
    batch
}

fn claude_message_text(value: &Value) -> String {
    match value.get("type").and_then(Value::as_str) {
        Some("user") => claude_user_text(value),
        Some("assistant") => claude_assistant_text(value),
        _ => String::new(),
    }
}

fn parse_claude_summary(
    path: &Path,
    fallback_workspace: Option<String>,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    let fallback_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    let mut id = None;
    let mut workspace = None;
    let mut model = None;
    let mut first_user = None;
    let mut custom_title = None;
    let mut ai_title = None;
    let mut started = None;
    let mut updated = None;
    let mut message_count = 0u32;
    for_each_jsonl(path, |value| {
        let record_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(record_type, "file-history-snapshot" | "progress")
            || bool_field(&value, "isMeta")
            || claude_interrupt_marker(&value)
        {
            return;
        }
        if record_type == "custom-title" {
            custom_title = string_field(&value, "customTitle").or(custom_title.take());
        } else if record_type == "ai-title" {
            ai_title = string_field(&value, "aiTitle").or(ai_title.take());
        }
        if id.is_none() {
            id = string_field(&value, "sessionId");
        }
        set_first_string(&mut workspace, string_field(&value, "cwd"));
        let timestamp = field_timestamp(&value, "timestamp");
        set_first_i64(&mut started, timestamp);
        set_last_i64(&mut updated, timestamp);
        if matches!(record_type, "user" | "assistant") {
            if record_type == "assistant"
                && (bool_field(&value, "isSynthetic")
                    || string_field(&value, "model").as_deref() == Some("<synthetic>")
                    || value.pointer("/message/model").and_then(Value::as_str)
                        == Some("<synthetic>"))
            {
                return;
            }
            let text = claude_message_text(&value);
            if record_type == "user" {
                if claude_tool_result_only(&value) || text.is_empty() {
                    return;
                }
                message_count = message_count.saturating_add(1);
                if !text.is_empty() {
                    set_first_string(&mut first_user, Some(title_from_text(&text)));
                }
            } else {
                if text.is_empty()
                    && value
                        .pointer("/message/content")
                        .and_then(Value::as_array)
                        .is_none_or(|parts| {
                            !parts.iter().any(|part| {
                                matches!(
                                    part.get("type").and_then(Value::as_str),
                                    Some("thinking" | "tool_use")
                                )
                            })
                        })
                {
                    return;
                }
                message_count = message_count.saturating_add(1);
                set_first_string(&mut model, string_field(&value, "model"));
                set_first_string(
                    &mut model,
                    value
                        .pointer("/message/model")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                );
            }
        }
    })?;
    Ok(build_summary(
        LocalHistorySource::Claude,
        id.unwrap_or(fallback_id),
        custom_title.or(ai_title).or(first_user),
        workspace.or(fallback_workspace),
        path,
        started,
        updated,
        message_count,
        model,
    ))
}

fn parse_claude_timeline(path: &Path) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let mut timeline = Vec::new();
    for_each_jsonl(path, |value| {
        let record_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(record_type, "file-history-snapshot" | "progress")
            || bool_field(&value, "isMeta")
            || claude_interrupt_marker(&value)
            || (record_type == "assistant"
                && (bool_field(&value, "isSynthetic")
                    || value.pointer("/message/model").and_then(Value::as_str)
                        == Some("<synthetic>")))
        {
            return;
        }
        let timestamp = field_timestamp(&value, "timestamp");
        match record_type {
            "user" => {
                if claude_context_continuation(&value) {
                    if let Some(entry) = system_entry(claude_user_text(&value), timestamp) {
                        timeline.push(entry);
                    }
                    return;
                }
                let text = claude_user_text(&value);
                if !claude_tool_result_only(&value) {
                    if let Some(entry) = user_entry(text, timestamp) {
                        timeline.push(entry);
                    }
                }
                if let Some(parts) = claude_message_content(&value).and_then(Value::as_array) {
                    for part in parts {
                        if matches!(
                            part.get("type").and_then(Value::as_str),
                            Some("tool_result" | "server_tool_result")
                        ) {
                            timeline.push(tool_entry(
                                string_field(part, "tool_use_id"),
                                Some("tool result".to_string()),
                                None,
                                json_preview(part.get("content")),
                                bool_field(part, "is_error"),
                                timestamp,
                            ));
                        }
                    }
                }
            }
            "assistant" => {
                let text = claude_assistant_text(&value);
                if let Some(entry) = agent_entry(text, timestamp) {
                    timeline.push(entry);
                }
                if let Some(parts) = claude_message_content(&value).and_then(Value::as_array) {
                    for part in parts {
                        match part.get("type").and_then(Value::as_str) {
                            Some("thinking") => {
                                if let Some(entry) = reasoning_entry(
                                    string_field(part, "thinking")
                                        .or_else(|| string_field(part, "text"))
                                        .unwrap_or_default(),
                                    timestamp,
                                ) {
                                    timeline.push(entry);
                                }
                            }
                            Some("tool_use") => timeline.push(tool_entry(
                                string_field(part, "id"),
                                string_field(part, "name"),
                                json_preview(part.get("input")),
                                None,
                                false,
                                timestamp,
                            )),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    })
    .map_err(local_history_read_error)?;
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

fn scan_codex(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let titles = load_codex_titles(&root.root);
    let mut paths = Vec::new();
    collect_codex_rollouts(&root_child(&root.root, "sessions"), 0, &mut paths);
    for path in paths.into_iter().take(MAX_SESSIONS_PER_SOURCE) {
        let parsed = cached_file_summary(LocalHistorySource::Codex, &path, || {
            parse_codex_summary(&path)
        })
        .map(|summary| {
            summary.map(|mut summary| {
                if let Some(title) = titles.get(&summary.key.external_id) {
                    summary.title = title.clone();
                }
                summary
            })
        });
        append_summary(
            &mut batch,
            LocalHistorySource::Codex,
            parsed,
            LocalHistoryLocator::Transcript(path),
        );
    }
    batch
}

fn collect_codex_rollouts(directory: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 6 || out.len() >= MAX_SESSIONS_PER_SOURCE {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_codex_rollouts(&path, depth + 1, out);
        } else if path.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-"))
        {
            out.push(path);
        }
    }
}

fn load_codex_titles(home: &Path) -> HashMap<String, String> {
    let mut titles = HashMap::new();
    let path = home.join("session_index.jsonl");
    let _ = for_each_jsonl(&path, |value| {
        if let (Some(id), Some(name)) = (
            string_field(&value, "id"),
            string_field(&value, "thread_name"),
        ) {
            titles.insert(id, bounded_text(&name, MAX_TITLE_CHARS));
        }
    });
    titles
}

fn parse_codex_summary(path: &Path) -> Result<Option<LocalHistorySessionSummary>, String> {
    let mut id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    let mut workspace = None;
    let mut model = None;
    let mut title = None;
    let mut started = None;
    let mut updated = None;
    let mut message_count = 0u32;
    for_each_jsonl(path, |value| {
        let record_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timestamp = field_timestamp(&value, "timestamp");
        match record_type {
            "session_meta" => {
                let Some(payload) = value.get("payload") else {
                    return;
                };
                id = string_field(payload, "id").unwrap_or(id.clone());
                set_first_string(&mut workspace, string_field(payload, "cwd"));
                set_first_i64(&mut started, timestamp);
                set_last_i64(&mut updated, timestamp);
            }
            "turn_context" => {
                set_first_string(
                    &mut model,
                    value
                        .pointer("/payload/model")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                );
            }
            "event_msg" => {
                let Some(payload) = value.get("payload") else {
                    return;
                };
                match payload.get("type").and_then(Value::as_str) {
                    Some("user_message") => {
                        let text = string_field(payload, "message").unwrap_or_default();
                        if !text.is_empty() {
                            message_count = message_count.saturating_add(1);
                            set_first_string(&mut title, Some(title_from_text(&text)));
                            set_first_i64(&mut started, timestamp);
                            set_last_i64(&mut updated, timestamp);
                        }
                    }
                    Some("agent_message") => {
                        message_count = message_count.saturating_add(1);
                        set_first_i64(&mut started, timestamp);
                        set_last_i64(&mut updated, timestamp);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    })?;
    Ok(build_summary(
        LocalHistorySource::Codex,
        id,
        title,
        workspace,
        path,
        started,
        updated,
        message_count,
        model,
    ))
}

fn parse_codex_timeline(path: &Path) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let mut timeline = Vec::new();
    for_each_jsonl(path, |value| {
        if value.get("type").and_then(Value::as_str) != Some("event_msg") {
            return;
        }
        let Some(payload) = value.get("payload") else {
            return;
        };
        let timestamp = field_timestamp(&value, "timestamp");
        match payload.get("type").and_then(Value::as_str) {
            Some("user_message") => {
                if let Some(entry) = user_entry(
                    string_field(payload, "message").unwrap_or_default(),
                    timestamp,
                ) {
                    timeline.push(entry);
                }
            }
            Some("agent_message") => {
                if let Some(entry) = agent_entry(
                    string_field(payload, "message").unwrap_or_default(),
                    timestamp,
                ) {
                    timeline.push(entry);
                }
            }
            _ => {}
        }
    })
    .map_err(local_history_read_error)?;
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// Gemini CLI
// ---------------------------------------------------------------------------

fn scan_gemini(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    for store in [root.root.join("tmp"), root.root.join("history")] {
        for alias_dir in direct_subdirs(&store) {
            let chats = alias_dir.join("chats");
            for path in direct_files(&chats) {
                let is_session = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("session-"));
                let supported = matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("json") | Some("jsonl")
                );
                if !is_session || !supported {
                    continue;
                }
                let parsed = parse_gemini_document(&path).and_then(|document| {
                    parse_gemini_summary(&root.root, &alias_dir, &path, &document)
                });
                append_summary(
                    &mut batch,
                    LocalHistorySource::Gemini,
                    parsed,
                    LocalHistoryLocator::Transcript(path),
                );
                if batch.found.len() >= MAX_SESSIONS_PER_SOURCE {
                    return batch;
                }
            }
        }
    }
    batch
}

fn parse_gemini_document(path: &Path) -> Result<Value, String> {
    let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
    if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
        return serde_json::from_str(&raw).map_err(|error| error.to_string());
    }
    let mut root = Map::new();
    let mut messages = Vec::new();
    let mut indices = HashMap::new();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let value = serde_json::from_str::<Value>(line).map_err(|error| error.to_string())?;
        let Some(object) = value.as_object() else {
            continue;
        };
        for key in [
            "kind",
            "sessionId",
            "projectHash",
            "startTime",
            "lastUpdated",
        ] {
            if let Some(field) = object.get(key) {
                root.insert(key.to_string(), field.clone());
            }
        }
        if let Some(set) = object.get("$set").and_then(Value::as_object) {
            if let Some(last_updated) = set.get("lastUpdated") {
                root.insert("lastUpdated".to_string(), last_updated.clone());
            }
        }
        if object.get("type").and_then(Value::as_str).is_none() {
            continue;
        }
        if let Some(id) = object.get("id").and_then(Value::as_str) {
            if let Some(index) = indices.get(id).copied() {
                merge_json_object(&mut messages[index], value);
                continue;
            }
            indices.insert(id.to_string(), messages.len());
        }
        messages.push(value);
    }
    root.insert("messages".to_string(), Value::Array(messages));
    Ok(Value::Object(root))
}

fn merge_json_object(existing: &mut Value, update: Value) {
    if let (Some(existing), Some(update)) = (existing.as_object_mut(), update.as_object()) {
        for (key, value) in update {
            existing.insert(key.clone(), value.clone());
        }
    } else {
        *existing = update;
    }
}

fn gemini_workspace(root: &Path, alias_dir: &Path) -> Option<String> {
    let alias = alias_dir.file_name()?.to_str()?;
    for candidate in [
        root.join("tmp").join(alias).join(".project_root"),
        root.join("history").join(alias).join(".project_root"),
    ] {
        if let Ok(value) = fs::read_to_string(candidate) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    let projects = json_file(&root.join("projects.json")).ok()?;
    projects
        .get("projects")
        .and_then(Value::as_object)
        .and_then(|projects| {
            projects.iter().find_map(|(path, mapped_alias)| {
                (mapped_alias.as_str() == Some(alias)).then(|| path.clone())
            })
        })
}

fn parse_gemini_summary(
    root: &Path,
    alias_dir: &Path,
    path: &Path,
    document: &Value,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    let id = string_field(document, "sessionId").or_else(|| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .map(ToOwned::to_owned)
    });
    let Some(id) = id else {
        return Ok(None);
    };
    let messages = document
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut title = None;
    let mut first_user = None;
    let mut model = None;
    let mut started = field_timestamp(document, "startTime");
    let mut updated = field_timestamp(document, "lastUpdated");
    let mut message_count = 0u32;
    for message in &messages {
        let timestamp = field_timestamp(message, "timestamp");
        set_first_i64(&mut started, timestamp);
        set_last_i64(&mut updated, timestamp);
        match message.get("type").and_then(Value::as_str) {
            Some("user") => {
                let text = text_string_or_parts(message.get("content"));
                if !text.is_empty() {
                    message_count = message_count.saturating_add(1);
                    if !text.trim_start().starts_with("<session_context") {
                        set_first_string(&mut first_user, Some(title_from_text(&text)));
                    }
                }
            }
            Some("gemini" | "assistant" | "model") => {
                message_count = message_count.saturating_add(1);
                set_last_string(&mut model, string_field(message, "model"));
            }
            _ => {}
        }
    }
    for message in messages.iter().rev() {
        let Some(tool_calls) = message.get("toolCalls").and_then(Value::as_array) else {
            continue;
        };
        if let Some(found) = tool_calls.iter().rev().find_map(|call| {
            (call.get("name").and_then(Value::as_str) == Some("update_topic"))
                .then(|| call.pointer("/args/title").and_then(Value::as_str))
                .flatten()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned)
        }) {
            title = Some(found);
            break;
        }
    }
    Ok(build_summary(
        LocalHistorySource::Gemini,
        id,
        title.or(first_user),
        gemini_workspace(root, alias_dir),
        path,
        started,
        updated,
        message_count,
        model,
    ))
}

fn parse_gemini_timeline(path: &Path) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let document = parse_gemini_document(path).map_err(local_history_read_error)?;
    let mut timeline = Vec::new();
    for message in document
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let timestamp = field_timestamp(message, "timestamp");
        match message.get("type").and_then(Value::as_str) {
            Some("user") => {
                let text = text_string_or_parts(message.get("content"));
                if !text.trim_start().starts_with("<session_context") {
                    if let Some(entry) = user_entry(text, timestamp) {
                        timeline.push(entry);
                    }
                }
            }
            Some("gemini" | "assistant" | "model") => {
                if let Some(entry) =
                    agent_entry(text_string_or_parts(message.get("content")), timestamp)
                {
                    timeline.push(entry);
                }
                for call in message
                    .get("toolCalls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let failed = matches!(
                        call.get("status").and_then(Value::as_str),
                        Some("error" | "failed")
                    );
                    timeline.push(tool_entry(
                        string_field(call, "id"),
                        string_field(call, "name"),
                        json_preview(call.get("args")),
                        json_preview(call.get("result"))
                            .or_else(|| json_preview(call.get("resultDisplay"))),
                        failed,
                        timestamp,
                    ));
                }
            }
            _ => {}
        }
    }
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// Cline
// ---------------------------------------------------------------------------

fn scan_cline(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let history = root.root.join("state").join("taskHistory.json");
    let Ok(entries) = json_file(&history).and_then(|value| {
        value
            .as_array()
            .cloned()
            .ok_or_else(|| "task history is not an array".to_string())
    }) else {
        return batch;
    };
    for entry in entries.into_iter().take(MAX_SESSIONS_PER_SOURCE) {
        let Some(task_id) = string_field(&entry, "id") else {
            continue;
        };
        let transcript = root
            .root
            .join("tasks")
            .join(&task_id)
            .join("api_conversation_history.json");
        if !transcript.is_file() {
            continue;
        }
        append_summary(
            &mut batch,
            LocalHistorySource::Cline,
            parse_cline_summary(&root.root, &entry, &transcript),
            LocalHistoryLocator::Cline {
                data_root: root.root.clone(),
                task_id,
            },
        );
    }
    batch
}

fn cline_user_texts(content: Option<&Value>) -> Vec<String> {
    let texts = match content {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                (part.get("type").and_then(Value::as_str).is_none()
                    || part.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| string_field(part, "text"))
                .flatten()
            })
            .collect(),
        _ => Vec::new(),
    };
    texts
        .into_iter()
        .filter_map(|text| {
            if let (Some(start), Some(end)) = (text.find("<feedback>"), text.find("</feedback>")) {
                let start = start + "<feedback>".len();
                return (end > start).then(|| text[start..end].trim().to_string());
            }
            let trimmed = text.trim();
            (!trimmed.starts_with('[') || !trimmed.contains("] Result:"))
                .then(|| trimmed.to_string())
        })
        .filter(|text| !text.is_empty())
        .collect()
}

/// Cline's task ids and `ts` fields are already Unix milliseconds. They are
/// intentionally kept separate from the generic timestamp normalizer, which
/// treats small numeric values as seconds for other providers.
fn cline_timestamp(value: &Value, key: &str) -> Option<i64> {
    let value = value.get(key)?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
}

fn parse_cline_summary(
    data_root: &Path,
    history_entry: &Value,
    transcript: &Path,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    let task_id = string_field(history_entry, "id").unwrap_or_default();
    let messages = json_file(transcript)?
        .as_array()
        .cloned()
        .ok_or_else(|| "conversation history is not an array".to_string())?;
    let message_count = messages.len().min(u32::MAX as usize) as u32;
    let mut first_user = None;
    let mut model = string_field(history_entry, "modelId");
    let started = task_id
        .parse::<i64>()
        .ok()
        .or_else(|| cline_timestamp(history_entry, "ts"));
    let updated = cline_timestamp(history_entry, "ts");
    for message in &messages {
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                set_first_string(
                    &mut model,
                    message
                        .pointer("/modelInfo/modelId")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                );
            }
            Some("user") => {
                for text in cline_user_texts(message.get("content")) {
                    set_first_string(&mut first_user, Some(title_from_text(&text)));
                }
            }
            _ => {}
        }
    }
    let title = string_field(history_entry, "task")
        .map(|title| title_from_text(&title))
        .or(first_user);
    let workspace = string_field(history_entry, "cwdOnTaskInitialization");
    let metadata = data_root
        .join("tasks")
        .join(&task_id)
        .join("task_metadata.json");
    if model.is_none() {
        model = json_file(&metadata).ok().and_then(|value| {
            value
                .pointer("/modelUsage/0/modelId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });
    }
    Ok(build_summary(
        LocalHistorySource::Cline,
        task_id,
        title,
        workspace,
        transcript,
        started,
        updated,
        message_count,
        model,
    ))
}

fn parse_cline_timeline(
    data_root: &Path,
    task_id: &str,
) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let transcript = data_root
        .join("tasks")
        .join(task_id)
        .join("api_conversation_history.json");
    let messages = json_file(&transcript)
        .map_err(local_history_read_error)?
        .as_array()
        .cloned()
        .ok_or_else(|| {
            local_history_read_error("conversation history is not an array".to_string())
        })?;
    let mut timeline = Vec::new();
    for message in messages {
        let timestamp = cline_timestamp(&message, "ts");
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                if let Some(entry) =
                    agent_entry(text_string_or_parts(message.get("content")), timestamp)
                {
                    timeline.push(entry);
                }
                for part in message
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if part.get("type").and_then(Value::as_str) == Some("tool_use") {
                        timeline.push(tool_entry(
                            string_field(part, "id"),
                            string_field(part, "name"),
                            json_preview(part.get("input")),
                            None,
                            false,
                            timestamp,
                        ));
                    }
                }
            }
            Some("user") => {
                for text in cline_user_texts(message.get("content")) {
                    if let Some(entry) = user_entry(text, timestamp) {
                        timeline.push(entry);
                    }
                }
            }
            _ => {}
        }
    }
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// OpenCode (SQLite)
// ---------------------------------------------------------------------------

fn scan_opencode(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let database = if root.root.is_file() {
        root.root.clone()
    } else {
        root.root.join("opencode.db")
    };
    let Ok(connection) = open_readonly(&database) else {
        return batch;
    };
    let query = r#"
        SELECT
            s.id,
            s.directory,
            s.title,
            s.time_created,
            s.time_updated,
            COALESCE((SELECT COUNT(*) FROM message m WHERE m.session_id = s.id), 0),
            (SELECT json_extract(m2.data, '$.modelID')
               FROM message m2
              WHERE m2.session_id = s.id
                AND json_extract(m2.data, '$.role') = 'assistant'
              ORDER BY m2.time_created DESC LIMIT 1),
            s.parent_id
          FROM session s
         ORDER BY s.time_created DESC
    "#;
    let Ok(mut statement) = connection.prepare(query) else {
        return batch;
    };
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    });
    let Ok(rows) = rows else {
        return batch;
    };
    for row in rows.flatten().take(MAX_SESSIONS_PER_SOURCE) {
        let (id, directory, title, created, updated, count, model, parent_id) = row;
        // Child rows represent delegated work and are not root import choices.
        if parent_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || count <= 0
        {
            continue;
        }
        let Some(summary) = build_summary(
            LocalHistorySource::OpenCode,
            id.clone(),
            title,
            directory,
            &database,
            Some(timestamp_number_to_ms(created)),
            Some(timestamp_number_to_ms(updated)),
            count.min(u32::MAX as i64) as u32,
            model,
        ) else {
            continue;
        };
        batch.found.push(FoundSession {
            summary,
            locator: LocalHistoryLocator::OpenCode {
                database: database.clone(),
                session_id: id,
            },
        });
    }
    batch
}

fn parse_opencode_summary(
    database: &Path,
    session_id: &str,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    let connection = open_readonly(database).map_err(|error| error.to_string())?;
    let query = r#"
        SELECT
            s.id,
            s.directory,
            s.title,
            s.time_created,
            s.time_updated,
            COALESCE((SELECT COUNT(*) FROM message m WHERE m.session_id = s.id), 0),
            (SELECT json_extract(m2.data, '$.modelID')
               FROM message m2
              WHERE m2.session_id = s.id
                AND json_extract(m2.data, '$.role') = 'assistant'
              ORDER BY m2.time_created DESC LIMIT 1),
            s.parent_id
          FROM session s
         WHERE s.id = ?1
         LIMIT 1
    "#;
    let row = connection
        .query_row(query, params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .optional()
        .map_err(|error| error.to_string())?;
    let Some((id, directory, title, created, updated, count, model, parent_id)) = row else {
        return Ok(None);
    };
    if parent_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(None);
    }
    Ok(build_summary(
        LocalHistorySource::OpenCode,
        id,
        title,
        directory,
        database,
        Some(timestamp_number_to_ms(created)),
        Some(timestamp_number_to_ms(updated)),
        count.max(0).min(u32::MAX as i64) as u32,
        model,
    ))
}

fn parse_opencode_timeline(
    database: &Path,
    session_id: &str,
) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let connection = open_readonly(database).map_err(|error| {
        VibexError::storage(
            "local_history_database_unreadable",
            "failed to open local history database",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let mut statement = connection
        .prepare(
            "SELECT data, time_created FROM message WHERE session_id = ?1 ORDER BY time_created ASC",
        )
        .map_err(|error| {
            VibexError::storage("local_history_timeline_query_failed", "failed to query local history database")
                .with_diagnostic("error", error.to_string())
        })?;
    let rows = statement
        .query_map(params![session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|error| {
            VibexError::storage(
                "local_history_timeline_query_failed",
                "failed to query local history database",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    let mut timeline = Vec::new();
    for row in rows.flatten() {
        let Ok(value) = serde_json::from_str::<Value>(&row.0) else {
            continue;
        };
        let timestamp = Some(timestamp_number_to_ms(row.1));
        let role = string_field(&value, "role").or_else(|| {
            value
                .pointer("/message/role")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });
        let content = value
            .get("content")
            .or_else(|| value.get("parts"))
            .or_else(|| value.pointer("/message/content"));
        match role.as_deref() {
            Some("user") => {
                if let Some(entry) = user_entry(text_string_or_parts(content), timestamp) {
                    timeline.push(entry);
                }
            }
            Some("assistant") | Some("model") => {
                if let Some(entry) = agent_entry(text_string_or_parts(content), timestamp) {
                    timeline.push(entry);
                }
                if let Some(calls) = value
                    .get("tool_calls")
                    .or_else(|| value.get("toolCalls"))
                    .and_then(Value::as_array)
                {
                    for call in calls {
                        timeline.push(tool_entry(
                            string_field(call, "id"),
                            call.pointer("/function/name")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned)
                                .or_else(|| string_field(call, "name")),
                            call.pointer("/function/arguments")
                                .and_then(|value| json_preview(Some(value)))
                                .or_else(|| json_preview(call.get("input"))),
                            None,
                            false,
                            timestamp,
                        ));
                    }
                }
            }
            Some("tool") => {
                timeline.push(tool_entry(
                    string_field(&value, "tool_call_id"),
                    string_field(&value, "name"),
                    None,
                    json_preview(content),
                    false,
                    timestamp,
                ));
            }
            _ => {}
        }
    }
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// Hermes (SQLite)
// ---------------------------------------------------------------------------

/// Hermes stores multimodal message content as a NUL-prefixed JSON parts
/// array. Keep the decoder local to the importer so malformed or future parts
/// degrade to readable text instead of making the whole session unavailable.
const HERMES_CONTENT_JSON_PREFIX: &str = "\0json:";

#[derive(Debug, Clone)]
enum HermesContentBlock {
    Text(String),
    Image,
}

fn decode_hermes_content(raw: Option<&str>) -> Vec<HermesContentBlock> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    if let Some(rest) = raw.strip_prefix(HERMES_CONTENT_JSON_PREFIX) {
        if let Ok(Value::Array(parts)) = serde_json::from_str::<Value>(rest) {
            return parts.iter().filter_map(hermes_content_part).collect();
        }
        let text = rest.trim();
        return (!text.is_empty())
            .then(|| HermesContentBlock::Text(text.to_string()))
            .into_iter()
            .collect();
    }
    let text = raw.trim();
    (!text.is_empty())
        .then(|| HermesContentBlock::Text(text.to_string()))
        .into_iter()
        .collect()
}

fn hermes_content_part(part: &Value) -> Option<HermesContentBlock> {
    if let Some(text) = part.as_str() {
        let text = text.trim();
        return (!text.is_empty()).then(|| HermesContentBlock::Text(text.to_string()));
    }
    let object = part.as_object()?;
    match object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "text" => object
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| HermesContentBlock::Text(text.to_string())),
        "image_url" | "image" | "input_image" => Some(HermesContentBlock::Image),
        _ => object
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| HermesContentBlock::Text(text.to_string())),
    }
}

fn content_to_text(raw: Option<&str>) -> Option<String> {
    let mut parts = Vec::new();
    for block in decode_hermes_content(raw) {
        match block {
            HermesContentBlock::Text(text) => parts.push(text),
            HermesContentBlock::Image => parts.push("[image]".to_string()),
        }
    }
    let text = parts.join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn normalize_tool_arguments(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        value => serde_json::to_string(value).ok(),
    }
}

fn parse_hermes_tool_calls(raw: &str) -> Vec<(Option<String>, String, Option<String>)> {
    let raw = raw
        .strip_prefix(HERMES_CONTENT_JSON_PREFIX)
        .unwrap_or(raw)
        .trim();
    let Ok(Value::Array(calls)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    calls
        .iter()
        .filter_map(|call| {
            let object = call.as_object()?;
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let function = object.get("function").and_then(Value::as_object);
            let name = function
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .or_else(|| object.get("name").and_then(Value::as_str))
                .unwrap_or("tool")
                .trim();
            if name.is_empty() {
                return None;
            }
            let input = function
                .and_then(|function| function.get("arguments"))
                .or_else(|| object.get("arguments"))
                .or_else(|| object.get("input"))
                .and_then(normalize_tool_arguments);
            Some((id, name.to_string(), input))
        })
        .collect()
}

fn scan_hermes(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let database = if root.root.is_file() {
        root.root.clone()
    } else {
        root.root.join("state.db")
    };
    let Ok(connection) = open_readonly(&database) else {
        return batch;
    };
    let query = r#"
        SELECT
            s.id,
            COALESCE(NULLIF(s.cwd, ''),
                CASE WHEN json_valid(s.model_config)
                     THEN json_extract(s.model_config, '$.cwd') END),
            s.title,
            s.model,
            s.started_at,
            s.ended_at,
            (SELECT COUNT(*) FROM messages m
              WHERE m.session_id = s.id AND m.active = 1 AND m.role <> 'system')
          FROM sessions s
         WHERE COALESCE(s.archived, 0) = 0
         ORDER BY s.started_at DESC
    "#;
    let Ok(mut statement) = connection.prepare(query) else {
        return batch;
    };
    let Ok(rows) = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<f64>>(4)?,
            row.get::<_, Option<f64>>(5)?,
            row.get::<_, i64>(6)?,
        ))
    }) else {
        return batch;
    };
    for row in rows.flatten().take(MAX_SESSIONS_PER_SOURCE) {
        let (id, cwd, title, model, started, ended, count) = row;
        if count <= 0 {
            continue;
        }
        let Some(summary) = build_summary(
            LocalHistorySource::Hermes,
            id.clone(),
            title,
            cwd,
            &database,
            started.map(seconds_number_to_ms),
            ended.map(seconds_number_to_ms),
            count.min(u32::MAX as i64) as u32,
            model,
        ) else {
            continue;
        };
        batch.found.push(FoundSession {
            summary,
            locator: LocalHistoryLocator::Hermes {
                database: database.clone(),
                session_id: id,
            },
        });
    }
    batch
}

fn parse_hermes_summary(
    database: &Path,
    session_id: &str,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    let connection = open_readonly(database).map_err(|error| error.to_string())?;
    let query = r#"
        SELECT
            s.id,
            COALESCE(NULLIF(s.cwd, ''),
                CASE WHEN json_valid(s.model_config)
                     THEN json_extract(s.model_config, '$.cwd') END),
            s.title,
            s.model,
            s.started_at,
            s.ended_at,
            (SELECT COUNT(*) FROM messages m
              WHERE m.session_id = s.id AND m.active = 1 AND m.role <> 'system')
          FROM sessions s
         WHERE s.id = ?1
         LIMIT 1
    "#;
    let row = connection
        .query_row(query, params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .optional()
        .map_err(|error| error.to_string())?;
    let Some((id, cwd, title, model, started, ended, count)) = row else {
        return Ok(None);
    };
    Ok(build_summary(
        LocalHistorySource::Hermes,
        id,
        title,
        cwd,
        database,
        started.map(seconds_number_to_ms),
        ended.map(seconds_number_to_ms),
        count.max(0).min(u32::MAX as i64) as u32,
        model,
    ))
}

fn parse_hermes_timeline(
    database: &Path,
    session_id: &str,
) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let connection = open_readonly(database).map_err(|error| {
        VibexError::storage(
            "local_history_database_unreadable",
            "failed to open local history database",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let mut statement = connection
        .prepare(
            "SELECT role, content, tool_call_id, tool_calls, tool_name, reasoning, reasoning_content, timestamp, finish_reason FROM messages WHERE session_id = ?1 AND active = 1 AND role <> 'system' ORDER BY id ASC",
        )
        .map_err(|error| {
            VibexError::storage("local_history_timeline_query_failed", "failed to query local history database")
                .with_diagnostic("error", error.to_string())
        })?;
    let rows = statement
        .query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<f64>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })
        .map_err(|error| {
            VibexError::storage(
                "local_history_timeline_query_failed",
                "failed to query local history database",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    let mut timeline = Vec::new();
    for row in rows.flatten() {
        let (
            role,
            content,
            tool_call_id,
            tool_calls,
            tool_name,
            reasoning,
            reasoning_content,
            timestamp,
            finish_reason,
        ) = row;
        let timestamp = timestamp.map(seconds_number_to_ms);
        match role.as_str() {
            "user" => {
                if let Some(entry) = user_entry(
                    content_to_text(content.as_deref()).unwrap_or_default(),
                    timestamp,
                ) {
                    timeline.push(entry);
                }
            }
            "assistant" => {
                if let Some(reasoning) = reasoning_content
                    .or(reasoning)
                    .and_then(|text| content_to_text(Some(&text)))
                    .and_then(|text| reasoning_entry(text, timestamp))
                {
                    timeline.push(reasoning);
                }
                if let Some(entry) = agent_entry(
                    content_to_text(content.as_deref()).unwrap_or_default(),
                    timestamp,
                ) {
                    timeline.push(entry);
                }
                if let Some(raw_calls) = tool_calls {
                    for (id, name, input) in parse_hermes_tool_calls(&raw_calls) {
                        timeline.push(tool_entry(
                            id.or_else(|| tool_call_id.clone()),
                            Some(name).or_else(|| tool_name.clone()),
                            input,
                            None,
                            false,
                            timestamp,
                        ));
                    }
                }
            }
            "tool" => timeline.push(tool_entry(
                tool_call_id,
                tool_name,
                None,
                content_to_text(content.as_deref()),
                finish_reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("error")),
                timestamp,
            )),
            _ => {}
        }
    }
    require_timeline(timeline)
}

fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

fn timestamp_number_to_ms(value: i64) -> i64 {
    if value.unsigned_abs() < 100_000_000_000 {
        value.saturating_mul(1000)
    } else {
        value
    }
}

fn seconds_number_to_ms(value: f64) -> i64 {
    let millis = value * 1000.0;
    if millis.is_finite() && millis >= i64::MIN as f64 && millis <= i64::MAX as f64 {
        millis as i64
    } else {
        0
    }
}

fn local_history_read_error(error: impl ToString) -> VibexError {
    VibexError::storage(
        "local_history_timeline_read_failed",
        "failed to read local history",
    )
    .with_diagnostic("error", bounded_text(&error.to_string(), 180))
}

// The local stores used by the database-backed Agents contain small protobuf
// messages, but Vibex only needs a handful of fields from each schema. This
// defensive reader keeps unknown fields aligned and lets the import remain
// compatible when a source adds fields without requiring generated code.
mod proto {
    #[derive(Clone, Debug)]
    pub enum Value {
        Varint(u64),
        Fixed64(u64),
        Bytes(Vec<u8>),
        Fixed32,
    }

    impl Value {
        pub fn bytes(&self) -> Option<&[u8]> {
            match self {
                Self::Bytes(value) => Some(value),
                _ => None,
            }
        }

        pub fn string(&self) -> Option<&str> {
            std::str::from_utf8(self.bytes()?).ok()
        }

        pub fn u64(&self) -> Option<u64> {
            match self {
                Self::Varint(value) => Some(*value),
                _ => None,
            }
        }

        pub fn f64(&self) -> Option<f64> {
            match self {
                Self::Fixed64(value) => Some(f64::from_bits(*value)),
                _ => None,
            }
        }
    }

    pub fn fields(mut bytes: &[u8]) -> Vec<(u32, Value)> {
        let mut output = Vec::new();
        while !bytes.is_empty() {
            let Some((key, rest)) = varint(bytes) else {
                break;
            };
            bytes = rest;
            let field = (key >> 3) as u32;
            if field == 0 {
                break;
            }
            match key & 7 {
                0 => {
                    let Some((value, rest)) = varint(bytes) else {
                        break;
                    };
                    bytes = rest;
                    output.push((field, Value::Varint(value)));
                }
                1 => {
                    if bytes.len() < 8 {
                        break;
                    }
                    let (value, rest) = bytes.split_at(8);
                    bytes = rest;
                    output.push((
                        field,
                        Value::Fixed64(u64::from_le_bytes(value.try_into().unwrap())),
                    ));
                }
                2 => {
                    let Some((length, rest)) = varint(bytes) else {
                        break;
                    };
                    bytes = rest;
                    let Ok(length) = usize::try_from(length) else {
                        break;
                    };
                    if length > bytes.len() {
                        break;
                    }
                    let (value, rest) = bytes.split_at(length);
                    bytes = rest;
                    output.push((field, Value::Bytes(value.to_vec())));
                }
                5 => {
                    if bytes.len() < 4 {
                        break;
                    }
                    bytes = &bytes[4..];
                    output.push((field, Value::Fixed32));
                }
                _ => break,
            }
        }
        output
    }

    fn varint(mut bytes: &[u8]) -> Option<(u64, &[u8])> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = *bytes.first()?;
            bytes = &bytes[1..];
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some((value, bytes));
            }
        }
        None
    }

    pub fn first_bytes(bytes: &[u8], field: u32) -> Option<Vec<u8>> {
        fields(bytes).into_iter().find_map(|(number, value)| {
            (number == field)
                .then(|| value.bytes().map(ToOwned::to_owned))
                .flatten()
        })
    }

    pub fn first_string(bytes: &[u8], field: u32) -> Option<String> {
        fields(bytes).into_iter().find_map(|(number, value)| {
            (number == field)
                .then(|| value.string().map(ToOwned::to_owned))
                .flatten()
        })
    }

    pub fn first_u64(bytes: &[u8], field: u32) -> Option<u64> {
        fields(bytes)
            .into_iter()
            .find_map(|(number, value)| (number == field).then(|| value.u64()).flatten())
    }

    pub fn messages(bytes: &[u8], field: u32) -> impl Iterator<Item = Vec<u8>> {
        fields(bytes)
            .into_iter()
            .filter_map(move |(number, value)| {
                (number == field)
                    .then(|| value.bytes().map(ToOwned::to_owned))
                    .flatten()
            })
    }
}

// ---------------------------------------------------------------------------
// CodeBuddy Code
// ---------------------------------------------------------------------------

fn scan_codebuddy(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let projects = root_child(&root.root, "projects");
    for project in direct_subdirs(&projects) {
        // Top-level files are sessions.  A session's nested `subagents/`
        // directory is intentionally never entered.
        for path in direct_files(&project)
            .into_iter()
            .filter(|path| is_jsonl(path))
        {
            let parsed = cached_file_summary(LocalHistorySource::CodeBuddy, &path, || {
                parse_codebuddy_summary(&path)
            });
            append_summary(
                &mut batch,
                LocalHistorySource::CodeBuddy,
                parsed,
                LocalHistoryLocator::Transcript(path),
            );
            if batch.found.len() >= MAX_SESSIONS_PER_SOURCE {
                return batch;
            }
        }
    }
    batch
}

fn codebuddy_record_text(value: &Value, block_type: &str) -> String {
    let Some(content) = value.get("content") else {
        return String::new();
    };
    match content {
        Value::String(text) => text.trim().to_string(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some(block_type))
            .filter_map(|part| string_field(part, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn codebuddy_title(value: &Value, key: &str) -> Option<String> {
    string_field(value, key).or_else(|| {
        value
            .pointer("/payload")
            .and_then(|payload| string_field(payload, key))
    })
}

fn parse_codebuddy_summary(path: &Path) -> Result<Option<LocalHistorySessionSummary>, String> {
    let mut id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    let mut workspace = None;
    let mut model = None;
    let mut custom = None;
    let mut ai = None;
    let mut topic = None;
    let mut first_user = None;
    let mut started = None;
    let mut updated = None;
    let mut message_count = 0u32;
    for_each_jsonl(path, |value| {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timestamp = field_timestamp(&value, "timestamp");
        if let Some(value_id) = string_field(&value, "sessionId") {
            id = value_id;
        }
        set_first_string(&mut workspace, string_field(&value, "cwd"));
        set_first_string(&mut model, string_field(&value, "model"));
        match kind {
            "custom-title" => custom = codebuddy_title(&value, "customTitle").or(custom.take()),
            "ai-title" => ai = codebuddy_title(&value, "aiTitle").or(ai.take()),
            "topic" => topic = codebuddy_title(&value, "topic").or(topic.take()),
            "message" => match value.get("role").and_then(Value::as_str) {
                Some("user") => {
                    let text = codebuddy_record_text(&value, "input_text");
                    if !text.is_empty() {
                        message_count = message_count.saturating_add(1);
                        set_first_string(&mut first_user, Some(title_from_text(&text)));
                        set_first_i64(&mut started, timestamp);
                        set_last_i64(&mut updated, timestamp);
                    }
                }
                Some("assistant") => {
                    message_count = message_count.saturating_add(1);
                    set_first_i64(&mut started, timestamp);
                    set_last_i64(&mut updated, timestamp);
                    set_first_string(&mut model, string_field(&value, "model"));
                }
                _ => {}
            },
            _ => {}
        }
    })?;
    Ok(build_summary(
        LocalHistorySource::CodeBuddy,
        id,
        custom.or(ai).or(topic).or(first_user),
        workspace,
        path,
        started,
        updated,
        message_count,
        model,
    ))
}

fn parse_codebuddy_timeline(path: &Path) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let mut timeline = Vec::new();
    for_each_jsonl(path, |value| {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timestamp = field_timestamp(&value, "timestamp");
        match kind {
            "message" => match value.get("role").and_then(Value::as_str) {
                Some("user") => {
                    if let Some(entry) =
                        user_entry(codebuddy_record_text(&value, "input_text"), timestamp)
                    {
                        timeline.push(entry);
                    }
                }
                Some("assistant") => {
                    if let Some(entry) =
                        agent_entry(codebuddy_record_text(&value, "output_text"), timestamp)
                    {
                        timeline.push(entry);
                    }
                }
                _ => {}
            },
            "reasoning" => {
                let reasoning_text = codebuddy_record_text(&value, "reasoning_text");
                let reasoning_text = if reasoning_text.is_empty() {
                    string_field(&value, "text").unwrap_or_default()
                } else {
                    reasoning_text
                };
                if let Some(entry) = reasoning_entry(reasoning_text, timestamp) {
                    timeline.push(entry);
                }
            }
            "function_call" => timeline.push(tool_entry(
                string_field(&value, "callId").or_else(|| string_field(&value, "id")),
                string_field(&value, "name").or_else(|| {
                    value
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                }),
                json_preview(value.get("arguments").or_else(|| value.get("input"))),
                None,
                false,
                timestamp,
            )),
            "function_call_result" => timeline.push(tool_entry(
                string_field(&value, "callId").or_else(|| string_field(&value, "id")),
                Some("tool result".to_string()),
                None,
                json_preview(value.get("output").or_else(|| value.get("result"))),
                bool_field(&value, "isError"),
                timestamp,
            )),
            _ => {}
        }
    })
    .map_err(local_history_read_error)?;
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// Kimi Code
// ---------------------------------------------------------------------------

fn scan_kimi(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let sessions = root_child(&root.root, "sessions");
    let work_dirs = direct_subdirs(&sessions);
    let index = load_kimi_work_dirs(&sessions);
    for bucket in work_dirs {
        for session_dir in direct_subdirs(&bucket) {
            let Some(session_id) = session_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            let wire = session_dir.join("agents").join("main").join("wire.jsonl");
            if !wire.is_file() {
                continue;
            }
            let cwd = index.get(&session_id).cloned();
            let parsed = parse_kimi_summary(&session_dir, &session_id, cwd);
            append_summary(
                &mut batch,
                LocalHistorySource::Kimi,
                parsed,
                LocalHistoryLocator::Transcript(wire),
            );
            if batch.found.len() >= MAX_SESSIONS_PER_SOURCE {
                return batch;
            }
        }
    }
    batch
}

fn load_kimi_work_dirs(sessions: &Path) -> HashMap<String, String> {
    let mut index = HashMap::new();
    let Some(home) = sessions.parent() else {
        return index;
    };
    let index_path = home.join("session_index.jsonl");
    let _ = for_each_jsonl(&index_path, |value| {
        if let (Some(id), Some(work_dir)) = (
            string_field(&value, "sessionId"),
            string_field(&value, "workDir"),
        ) {
            index.insert(id, work_dir);
        }
    });
    index
}

fn kimi_event_object(value: &Value) -> &Value {
    value.get("event").unwrap_or(value)
}

fn kimi_prompt_text(value: &Value) -> String {
    text_string_or_parts(value.pointer("/input"))
}

fn parse_kimi_summary(
    session_dir: &Path,
    session_id: &str,
    workspace: Option<String>,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    let wire = session_dir.join("agents").join("main").join("wire.jsonl");
    let mut started = None;
    let mut updated = None;
    let mut first_user = None;
    let mut title = None;
    let mut model = None;
    let mut model_alias = None;
    let mut count = 0u32;
    for_each_jsonl(&wire, |value| {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timestamp = field_timestamp(&value, "time");
        match kind {
            "config.update" => {
                set_first_string(&mut model_alias, string_field(&value, "modelAlias"));
            }
            "turn.prompt" => {
                let text = kimi_prompt_text(&value);
                if !text.is_empty() {
                    count = count.saturating_add(1);
                    set_first_string(&mut first_user, Some(title_from_text(&text)));
                    set_first_i64(&mut started, timestamp);
                    set_last_i64(&mut updated, timestamp);
                }
            }
            "context.append_loop_event" => {
                let event = kimi_event_object(&value);
                match event.get("type").and_then(Value::as_str) {
                    Some("content.part") => {
                        let part = event.get("part").unwrap_or(event);
                        if part.get("type").and_then(Value::as_str) == Some("text")
                            && !string_field(part, "text").is_none_or(|text| text.is_empty())
                        {
                            count = count.saturating_add(1);
                            set_first_i64(&mut started, timestamp);
                            set_last_i64(&mut updated, timestamp);
                        }
                    }
                    Some("session.title") => title = string_field(event, "title").or(title.take()),
                    Some("request.header") => model = string_field(event, "model").or(model.take()),
                    _ => {}
                }
            }
            "session.title" => title = string_field(&value, "title").or(title.take()),
            "request.header" => model = string_field(&value, "model").or(model.take()),
            _ => {}
        }
    })?;
    let title = read_kimi_state_title(session_dir).or(title).or(first_user);
    let model = read_kimi_session_log_model(session_dir)
        .or(model)
        .or(model_alias);
    Ok(build_summary(
        LocalHistorySource::Kimi,
        session_id.to_string(),
        title,
        workspace,
        &wire,
        started,
        updated,
        count,
        model,
    ))
}

fn read_kimi_state_title(session_dir: &Path) -> Option<String> {
    let value = json_file(&session_dir.join("state.json")).ok()?;
    string_field(&value, "title").filter(|title| title != "New Session")
}

fn read_kimi_session_log_model(session_dir: &Path) -> Option<String> {
    let raw = fs::read_to_string(session_dir.join("logs").join("kimi-code.log")).ok()?;
    raw.lines()
        .filter(|line| line.contains("llm config"))
        .flat_map(|line| line.split_whitespace())
        .find_map(|token| token.strip_prefix("model=").map(str::trim))
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_kimi_timeline(path: &Path) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let mut timeline = Vec::new();
    for_each_jsonl(path, |value| {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timestamp = field_timestamp(&value, "time");
        match kind {
            "turn.prompt" => {
                if let Some(entry) = user_entry(kimi_prompt_text(&value), timestamp) {
                    timeline.push(entry);
                }
            }
            "context.append_loop_event" => {
                let event = kimi_event_object(&value);
                match event.get("type").and_then(Value::as_str) {
                    Some("content.part") => {
                        let part = event.get("part").unwrap_or(event);
                        match part.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(entry) = agent_entry(
                                    string_field(part, "text").unwrap_or_default(),
                                    timestamp,
                                ) {
                                    timeline.push(entry);
                                }
                            }
                            Some("think") => {
                                if let Some(entry) = reasoning_entry(
                                    string_field(part, "think").unwrap_or_default(),
                                    timestamp,
                                ) {
                                    timeline.push(entry);
                                }
                            }
                            _ => {}
                        }
                    }
                    Some("tool.call") => timeline.push(tool_entry(
                        string_field(event, "toolCallId"),
                        string_field(event, "name"),
                        json_preview(event.get("args")),
                        None,
                        false,
                        timestamp,
                    )),
                    Some("tool.result") => timeline.push(tool_entry(
                        string_field(event, "toolCallId"),
                        Some("tool result".to_string()),
                        None,
                        json_preview(event.pointer("/result/output")),
                        event
                            .pointer("/result/isError")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        timestamp,
                    )),
                    _ => {}
                }
            }
            _ => {}
        }
    })
    .map_err(local_history_read_error)?;
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// Pi coding agent
// ---------------------------------------------------------------------------

fn scan_pi(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let mut paths = Vec::new();
    collect_jsonl_files(&root.root, 0, 6, &mut paths);
    for path in paths.into_iter().take(MAX_SESSIONS_PER_SOURCE) {
        let parsed = cached_file_summary(LocalHistorySource::Pi, &path, || parse_pi_summary(&path));
        append_summary(
            &mut batch,
            LocalHistorySource::Pi,
            parsed,
            LocalHistoryLocator::Transcript(path),
        );
    }
    batch
}

fn collect_jsonl_files(directory: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
    if depth > max_depth || out.len() >= MAX_SESSIONS_PER_SOURCE {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, depth + 1, max_depth, out);
        } else if path.is_file() && is_jsonl(&path) {
            out.push(path);
        }
    }
}

fn pi_message_text(message: &Value) -> String {
    text_string_or_parts(message.get("content"))
}

fn parse_pi_summary(path: &Path) -> Result<Option<LocalHistorySessionSummary>, String> {
    let mut id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    let mut workspace = None;
    let mut title = None;
    let mut first_user = None;
    let mut model = None;
    let mut started = None;
    let mut updated = None;
    let mut count = 0u32;
    for_each_jsonl(path, |value| {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timestamp = field_timestamp(&value, "timestamp");
        match kind {
            "session" => {
                id = string_field(&value, "id").unwrap_or(id.clone());
                set_first_string(&mut workspace, string_field(&value, "cwd"));
                set_first_i64(&mut started, timestamp);
                set_last_i64(&mut updated, timestamp);
            }
            "session_info" => title = string_field(&value, "name").or(title.take()),
            "model_change" => model = string_field(&value, "modelId").or(model.take()),
            "message" => {
                let Some(message) = value.get("message") else {
                    return;
                };
                match message.get("role").and_then(Value::as_str) {
                    Some("user") | Some("assistant") => {
                        let text = pi_message_text(message);
                        if !text.is_empty() || message.get("content").is_some() {
                            count = count.saturating_add(1);
                            set_first_i64(&mut started, timestamp);
                            set_last_i64(&mut updated, timestamp);
                            if message.get("role").and_then(Value::as_str) == Some("user")
                                && !text.is_empty()
                            {
                                set_first_string(&mut first_user, Some(title_from_text(&text)));
                            }
                        }
                    }
                    _ => {}
                }
                set_first_string(&mut model, string_field(message, "model"));
            }
            _ => {}
        }
    })?;
    Ok(build_summary(
        LocalHistorySource::Pi,
        id,
        title.or(first_user),
        workspace,
        path,
        started,
        updated,
        count,
        model,
    ))
}

fn parse_pi_timeline(path: &Path) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let mut timeline = Vec::new();
    for_each_jsonl(path, |value| {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timestamp = field_timestamp(&value, "timestamp");
        match kind {
            "message" => {
                let Some(message) = value.get("message") else {
                    return;
                };
                match message.get("role").and_then(Value::as_str) {
                    Some("user") => {
                        if let Some(entry) = user_entry(pi_message_text(message), timestamp) {
                            timeline.push(entry);
                        }
                    }
                    Some("assistant") => {
                        if let Some(entry) = agent_entry(pi_message_text(message), timestamp) {
                            timeline.push(entry);
                        }
                        for part in message
                            .get("content")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            match part.get("type").and_then(Value::as_str) {
                                Some("thinking") => {
                                    if let Some(entry) = reasoning_entry(
                                        string_field(part, "text").unwrap_or_default(),
                                        timestamp,
                                    ) {
                                        timeline.push(entry);
                                    }
                                }
                                Some("toolCall") => timeline.push(tool_entry(
                                    string_field(part, "id")
                                        .or_else(|| string_field(part, "toolCallId")),
                                    string_field(part, "name"),
                                    json_preview(
                                        part.get("arguments").or_else(|| part.get("input")),
                                    ),
                                    None,
                                    false,
                                    timestamp,
                                )),
                                _ => {}
                            }
                        }
                    }
                    Some("toolResult") => timeline.push(tool_entry(
                        string_field(message, "toolCallId"),
                        string_field(message, "toolName")
                            .or_else(|| Some("tool result".to_string())),
                        None,
                        json_preview(message.get("content")),
                        bool_field(message, "isError"),
                        timestamp,
                    )),
                    _ => {}
                }
            }
            "bashExecution" => {
                timeline.push(tool_entry(
                    string_field(&value, "id"),
                    Some("bash".to_string()),
                    string_field(&value, "command"),
                    string_field(&value, "output"),
                    value
                        .get("exitCode")
                        .and_then(Value::as_i64)
                        .is_some_and(|code| code != 0),
                    timestamp,
                ));
            }
            _ => {}
        }
    })
    .map_err(local_history_read_error)?;
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// Grok
// ---------------------------------------------------------------------------

fn scan_grok(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let sessions = root_child(&root.root, "sessions");
    for group in direct_subdirs(&sessions) {
        for session_dir in direct_subdirs(&group) {
            let summary_file = session_dir.join("summary.json");
            let updates = session_dir.join("updates.jsonl");
            if !summary_file.is_file() || !updates.is_file() {
                continue;
            }
            let Some(id) = session_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            let parsed = parse_grok_summary(&session_dir, &id);
            append_summary(
                &mut batch,
                LocalHistorySource::Grok,
                parsed,
                LocalHistoryLocator::Transcript(updates),
            );
            if batch.found.len() >= MAX_SESSIONS_PER_SOURCE {
                return batch;
            }
        }
    }
    batch
}

fn grok_meta(session_dir: &Path) -> Option<Value> {
    json_file(&session_dir.join("summary.json")).ok()
}

fn grok_update_value(value: &Value) -> Option<&Value> {
    value
        .pointer("/params/update")
        .or_else(|| value.get("update"))
}

fn grok_update_kind(value: &Value) -> Option<&str> {
    grok_update_value(value)
        .and_then(|update| update.get("sessionUpdate"))
        .and_then(Value::as_str)
}

fn grok_update_text(value: &Value) -> String {
    grok_update_value(value)
        .and_then(|update| update.pointer("/content/text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn parse_grok_summary(
    session_dir: &Path,
    session_id: &str,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    let meta =
        grok_meta(session_dir).ok_or_else(|| "summary metadata is unreadable".to_string())?;
    if string_field(&meta, "session_kind").as_deref() == Some("subagent") {
        return Ok(None);
    }
    let mut title =
        string_field(&meta, "generated_title").or_else(|| string_field(&meta, "session_summary"));
    let mut workspace = meta
        .pointer("/info/cwd")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let mut model = string_field(&meta, "current_model_id");
    let mut started = field_timestamp(&meta, "created_at");
    let mut updated = field_timestamp(&meta, "updated_at");
    let mut first_user = None;
    let mut count = 0u32;
    for_each_jsonl(&session_dir.join("updates.jsonl"), |value| {
        let kind = grok_update_kind(&value).unwrap_or_default();
        let timestamp = field_timestamp(&value, "timestamp");
        match kind {
            "user_message_chunk" => {
                let text = grok_update_text(&value);
                let hidden = grok_update_value(&value)
                    .and_then(|update| update.pointer("/_meta/hideFromScrollback"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !hidden && !text.is_empty() {
                    count = count.saturating_add(1);
                    set_first_string(&mut first_user, Some(title_from_text(&text)));
                    set_first_i64(&mut started, timestamp);
                    set_last_i64(&mut updated, timestamp);
                }
            }
            "agent_message_chunk" => {
                if !grok_update_text(&value).is_empty() {
                    count = count.saturating_add(1);
                    set_first_i64(&mut started, timestamp);
                    set_last_i64(&mut updated, timestamp);
                }
            }
            "tool_call" | "tool_call_update" => {
                set_first_i64(&mut started, timestamp);
                set_last_i64(&mut updated, timestamp);
            }
            _ => {}
        }
        if model.is_none() {
            set_first_string(
                &mut model,
                grok_update_value(&value)
                    .and_then(|update| update.pointer("/_meta/modelId"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            );
        }
        if workspace.is_none() {
            set_first_string(
                &mut workspace,
                grok_update_value(&value)
                    .and_then(|update| update.get("cwd"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            );
        }
    })?;
    Ok(build_summary(
        LocalHistorySource::Grok,
        session_id.to_string(),
        title.take().or(first_user),
        workspace,
        &session_dir.join("updates.jsonl"),
        started,
        updated,
        count,
        model,
    ))
}

fn parse_grok_timeline(path: &Path) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let mut timeline = Vec::new();
    for_each_jsonl(path, |value| {
        let kind = grok_update_kind(&value).unwrap_or_default();
        let update = grok_update_value(&value);
        let timestamp = field_timestamp(&value, "timestamp");
        match kind {
            "user_message_chunk" => {
                let hidden = update
                    .and_then(|update| update.pointer("/_meta/hideFromScrollback"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !hidden {
                    if let Some(entry) = user_entry(grok_update_text(&value), timestamp) {
                        timeline.push(entry);
                    }
                }
            }
            "agent_message_chunk" => {
                if let Some(entry) = agent_entry(grok_update_text(&value), timestamp) {
                    timeline.push(entry);
                }
            }
            "agent_thought_chunk" => {
                if let Some(entry) = reasoning_entry(grok_update_text(&value), timestamp) {
                    timeline.push(entry);
                }
            }
            "tool_call" => {
                if let Some(update) = update {
                    timeline.push(tool_entry(
                        string_field(update, "toolCallId"),
                        string_field(update, "title").or_else(|| {
                            update
                                .pointer("/_meta/x.ai/tool/name")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned)
                        }),
                        json_preview(update.get("rawInput")),
                        None,
                        false,
                        timestamp,
                    ));
                }
            }
            "tool_call_update" => {
                if let Some(update) = update {
                    let status = string_field(update, "status");
                    let output = update_tool_output(update);
                    timeline.push(tool_entry(
                        string_field(update, "toolCallId"),
                        string_field(update, "title").or_else(|| Some("tool result".to_string())),
                        None,
                        output,
                        status
                            .as_deref()
                            .is_some_and(|status| matches!(status, "failed" | "error")),
                        timestamp,
                    ));
                }
            }
            _ => {}
        }
    })
    .map_err(local_history_read_error)?;
    require_timeline(timeline)
}

fn update_tool_output(update: &Value) -> Option<String> {
    if let Some(raw) = update.get("rawOutput") {
        return json_preview(Some(raw));
    }
    update
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.pointer("/content/text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| !text.trim().is_empty())
}

// ---------------------------------------------------------------------------
// DeepSeek Harness
// ---------------------------------------------------------------------------

fn scan_deepseek(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let attachments_root = deepseek_attachments_root_for_scan(&root.root);
    for bucket in direct_subdirs(&root.root) {
        for session_dir in direct_subdirs(&bucket) {
            let plain = session_dir.join("session.jsonl");
            let compressed = session_dir.join("session.jsonl.zstd");
            let path = if compressed.is_file() {
                compressed
            } else if plain.is_file() {
                plain
            } else {
                continue;
            };
            let Some(id) = session_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            let parsed = parse_deepseek_summary(&path, &id);
            append_summary(
                &mut batch,
                LocalHistorySource::DeepSeek,
                parsed,
                LocalHistoryLocator::DeepSeek {
                    session_dir,
                    attachments_root: attachments_root.clone(),
                },
            );
        }
    }
    batch
}

/// Resolve the attachment store for a scan root. Production DeepSeek moves
/// its logs with `DEEPSEEK_ACP_SESSIONS_ROOT` but keeps attachments below
/// `DSH_HOME`; test and caller-supplied roots are self-contained fixtures and
/// therefore keep their attachment objects beside the supplied sessions root.
fn deepseek_attachments_root_for_scan(sessions_root: &Path) -> PathBuf {
    let configured_sessions_root = resolve_deepseek_sessions_root_from(
        std::env::var_os("DEEPSEEK_ACP_SESSIONS_ROOT"),
        std::env::var_os("DSH_HOME"),
        dirs::home_dir(),
    );
    if sessions_root == configured_sessions_root {
        resolve_dsh_home_from(std::env::var_os("DSH_HOME"), dirs::home_dir())
            .join("attachments")
            .join("v1")
    } else {
        sessions_root.join("attachments").join("v1")
    }
}

fn deepseek_is_user(value: &Value) -> bool {
    value.pointer("/data/source/kind").and_then(Value::as_str) == Some("user")
        || value.pointer("/data/source").and_then(Value::as_str) == Some("user")
}

fn deepseek_user_content_from_event(
    event: &Value,
    attachments: Option<&Path>,
) -> (String, Vec<MessageAttachment>) {
    let content = event
        .pointer("/data/content")
        .or_else(|| event.pointer("/data/message/content"));
    deepseek_user_content(content, attachments)
}

fn deepseek_events(path: &Path) -> Result<Vec<Value>, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let text = if path.extension().and_then(|extension| extension.to_str()) == Some("zstd") {
        let mut decoder = zstd::stream::read::Decoder::with_buffer(bytes.as_slice())
            .map_err(|error| error.to_string())?;
        let mut decoded = Vec::new();
        let decode_result = decoder.read_to_end(&mut decoded);
        if decoded.is_empty() && decode_result.is_err() {
            return Err(decode_result
                .expect_err("zstd result is an error")
                .to_string());
        }
        let text = String::from_utf8_lossy(&decoded).into_owned();
        match decode_result {
            Ok(_) => text,
            // A torn tail frame corrupts the last partial line it touches;
            // keep only the lines the decoder completed before the error.
            Err(_) => match text.rfind('\n') {
                Some(index) => text[..=index].to_string(),
                None => String::new(),
            },
        }
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect())
}

fn deepseek_image_attachment(
    item: &Value,
    attachments: Option<&Path>,
) -> Option<MessageAttachment> {
    let attachment = item.get("attachment").unwrap_or(item);
    let mime = string_field(attachment, "mediaType");
    let name = string_field(attachment, "name");
    let label = name
        .clone()
        .or_else(|| mime.clone())
        .unwrap_or_else(|| "image".to_string());
    let mut uri = None;
    if let (Some(root), Some(id), Some(mime)) = (
        attachments,
        string_field(attachment, "attachmentId"),
        mime.as_deref(),
    ) {
        if let Some(hex) = id
            .strip_prefix("sha256:")
            .filter(|value| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            let object = root.join("objects").join(&hex[..2]).join(hex);
            if let Ok(metadata) = fs::metadata(&object) {
                if metadata.len() > 0 && metadata.len() <= 8 * 1024 * 1024 {
                    if let Ok(bytes) = fs::read(object) {
                        if bytes.len() <= 8 * 1024 * 1024 {
                            use base64::{Engine as _, engine::general_purpose::STANDARD};
                            uri = Some(format!("data:{mime};base64,{}", STANDARD.encode(bytes)));
                        }
                    }
                }
            }
        }
    }
    Some(MessageAttachment {
        label,
        mime_type: mime,
        uri,
        inline_text_offset: None,
    })
}

fn deepseek_user_content(
    content: Option<&Value>,
    attachments: Option<&Path>,
) -> (String, Vec<MessageAttachment>) {
    let mut text = String::new();
    let mut images = Vec::new();
    if let Some(items) = content.and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str).unwrap_or_default() {
                "text" => {
                    if let Some(value) = item.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(value);
                    }
                }
                "image" => {
                    if let Some(image) = deepseek_image_attachment(item, attachments) {
                        images.push(image);
                    }
                }
                _ => {}
            }
        }
    } else {
        text = text_string_or_parts(content);
    }
    (text.trim().to_string(), images)
}

fn deepseek_assistant_has_text(event: &Value) -> bool {
    event
        .pointer("/data/message/content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("text")
                    && string_field(block, "text").is_some()
            })
        })
}

fn deepseek_tool_result(value: &Value) -> (Option<String>, Option<String>, bool) {
    let data = value.get("data").unwrap_or(value);
    let result = data
        .pointer("/message/content")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find(|item| {
                matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("tool-result") | Some("tool_result")
                )
            })
        });
    let output = result
        .and_then(|item| item.get("content"))
        .map(|value| text_string_or_parts(Some(value)))
        .or_else(|| {
            data.get("output")
                .map(|value| text_string_or_parts(Some(value)))
        })
        .filter(|text| !text.trim().is_empty());
    let id = result
        .and_then(|item| string_field(item, "toolCallId"))
        .or_else(|| {
            data.pointer("/message/source/callId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| string_field(data, "toolCallId"));
    let failed = result
        .and_then(|item| item.get("isError").and_then(Value::as_bool))
        .unwrap_or(false)
        || data.get("error").is_some_and(|error| !error.is_null());
    (id, output, failed)
}

fn parse_deepseek_summary(
    path: &Path,
    session_id: &str,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    let events = deepseek_events(path)?;
    let mut workspace = None;
    let mut model = None;
    let mut title = None;
    let mut first_user = None;
    let mut started = None;
    let mut updated = None;
    let mut count = 0u32;
    let mut delegation_depth = 0u64;
    for event in &events {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timestamp = field_timestamp(event, "time");
        if kind == "session" {
            set_last_string(&mut workspace, string_field(event, "cwd"));
            delegation_depth = event
                .get("delegationDepth")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            set_first_i64(&mut started, timestamp);
            continue;
        }
        if kind == "session/title" {
            title = string_field(event, "title").or(title.take());
            continue;
        }
        if kind == "request/header" {
            model = event
                .pointer("/data/header/config/model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .or(model.take());
            continue;
        }
        if kind != "user/message" && kind != "assistant/message" {
            continue;
        }
        if kind == "user/message" && !deepseek_is_user(event) {
            continue;
        }
        let (text, images) = deepseek_user_content_from_event(event, None);
        let has_content = !text.is_empty() || !images.is_empty();
        if !has_content {
            continue;
        }
        if kind == "user/message" && has_content {
            count = count.saturating_add(1);
            if !text.is_empty() {
                set_first_string(&mut first_user, Some(title_from_text(&text)));
            }
        } else if kind == "assistant/message" && deepseek_assistant_has_text(event) {
            count = count.saturating_add(1);
        }
        set_first_i64(&mut started, timestamp);
        set_last_i64(&mut updated, timestamp);
        set_first_string(
            &mut model,
            event
                .pointer("/data/message/source/model")
                .or_else(|| event.pointer("/data/message/model"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        );
    }
    if delegation_depth > 0 {
        return Ok(None);
    }
    Ok(build_summary(
        LocalHistorySource::DeepSeek,
        session_id.to_string(),
        title.or(first_user),
        workspace,
        path,
        started,
        updated,
        count,
        model,
    ))
}

fn parse_deepseek_timeline(
    session_dir: &Path,
    attachments_root: &Path,
) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let path = if session_dir.join("session.jsonl.zstd").is_file() {
        session_dir.join("session.jsonl.zstd")
    } else {
        session_dir.join("session.jsonl")
    };
    let events = deepseek_events(&path).map_err(local_history_read_error)?;
    let mut timeline = Vec::new();
    for event in events {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let timestamp = field_timestamp(&event, "time");
        match kind {
            "user/message" if deepseek_is_user(&event) => {
                let (text, images) =
                    deepseek_user_content_from_event(&event, Some(attachments_root));
                if let Some(entry) = user_entry_with_attachments(text, images, timestamp) {
                    timeline.push(entry);
                }
            }
            "assistant/message" => {
                let Some(blocks) = event
                    .pointer("/data/message/content")
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                for block in blocks {
                    match block
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                    {
                        "text" => {
                            if let Some(entry) = agent_entry(
                                string_field(block, "text").unwrap_or_default(),
                                timestamp,
                            ) {
                                timeline.push(entry);
                            }
                        }
                        "reasoning" => {
                            if let Some(entry) = reasoning_entry(
                                string_field(block, "text").unwrap_or_default(),
                                timestamp,
                            ) {
                                timeline.push(entry);
                            }
                        }
                        "tool-call" | "tool_call" => timeline.push(tool_entry(
                            string_field(block, "id"),
                            string_field(block, "name"),
                            block
                                .get("arguments")
                                .and_then(|value| value.as_str())
                                .map(ToOwned::to_owned)
                                .or_else(|| json_preview(block.get("arguments"))),
                            None,
                            false,
                            timestamp,
                        )),
                        _ => {}
                    }
                }
            }
            "tool/result" => {
                let (id, output, failed) = deepseek_tool_result(&event);
                timeline.push(tool_entry(
                    id,
                    Some("tool result".to_string()),
                    None,
                    output,
                    failed,
                    timestamp,
                ));
            }
            "compaction/end" => {
                let id = event
                    .pointer("/data/compactionId")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                timeline.push(tool_entry(
                    id,
                    Some("context_compaction".to_string()),
                    None,
                    None,
                    event
                        .pointer("/data/error")
                        .is_some_and(|error| !error.is_null()),
                    timestamp,
                ));
            }
            _ => {}
        }
    }
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// ZCode (SQLite)
// ---------------------------------------------------------------------------

fn scan_zcode(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let database = if root.root.is_file() {
        root.root.clone()
    } else {
        root.root.join("cli").join("db").join("db.sqlite")
    };
    let Ok(connection) = open_readonly(&database) else {
        return batch;
    };
    let Ok(mut statement) = connection.prepare(&format!(
        "{ZCODE_SESSION_QUERY} ORDER BY s.time_created DESC"
    )) else {
        return batch;
    };
    let Ok(rows) = statement.query_map([], zcode_session_row) else {
        return batch;
    };
    for row in rows.flatten().take(MAX_SESSIONS_PER_SOURCE) {
        let (id, directory, title, created, updated, count, model, parent_id) = row;
        // Child rows represent delegated work and are not root import choices.
        if parent_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || count <= 0
        {
            continue;
        }
        let Some(summary) = build_summary(
            LocalHistorySource::Zcode,
            id.clone(),
            title,
            directory,
            &database,
            Some(timestamp_number_to_ms(created)),
            Some(timestamp_number_to_ms(updated)),
            count.min(u32::MAX as i64) as u32,
            model,
        ) else {
            continue;
        };
        batch.found.push(FoundSession {
            summary,
            locator: LocalHistoryLocator::Zcode {
                database: database.clone(),
                session_id: id,
            },
        });
    }
    batch
}

const ZCODE_SESSION_QUERY: &str = r#"
    SELECT
        s.id,
        s.directory,
        s.title,
        s.time_created,
        s.time_updated,
        COALESCE((SELECT COUNT(*) FROM message m WHERE m.session_id = s.id), 0),
        (SELECT json_extract(m2.data, '$.modelID')
           FROM message m2
          WHERE m2.session_id = s.id
            AND json_extract(m2.data, '$.role') = 'assistant'
          ORDER BY m2.time_created DESC LIMIT 1),
        s.parent_id
      FROM session s
"#;

fn zcode_session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ZcodeSessionRecord> {
    Ok((
        row.get::<_, String>(0)?,
        row.get::<_, Option<String>>(1)?,
        row.get::<_, Option<String>>(2)?,
        row.get::<_, i64>(3)?,
        row.get::<_, i64>(4)?,
        row.get::<_, i64>(5)?,
        row.get::<_, Option<String>>(6)?,
        row.get::<_, Option<String>>(7)?,
    ))
}

type ZcodeSessionRecord = (
    String,
    Option<String>,
    Option<String>,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
);

fn parse_zcode_summary(
    database: &Path,
    session_id: &str,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    let connection = open_readonly(database).map_err(|error| error.to_string())?;
    let query = format!("{ZCODE_SESSION_QUERY} WHERE s.id = ?1 LIMIT 1");
    let row = connection
        .query_row(&query, params![session_id], zcode_session_row)
        .optional()
        .map_err(|error| error.to_string())?;
    let Some((id, directory, title, created, updated, count, model, parent_id)) = row else {
        return Ok(None);
    };
    if parent_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(None);
    }
    Ok(build_summary(
        LocalHistorySource::Zcode,
        id,
        title,
        directory,
        database,
        Some(timestamp_number_to_ms(created)),
        Some(timestamp_number_to_ms(updated)),
        count.max(0).min(u32::MAX as i64) as u32,
        model,
    ))
}

fn parse_zcode_timeline(
    database: &Path,
    session_id: &str,
) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let connection = open_readonly(database).map_err(|error| {
        VibexError::storage(
            "local_history_database_unreadable",
            "failed to open local history database",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let mut statement = connection
        .prepare(
            "SELECT m.data, p.data, p.time_created
               FROM part p
               JOIN message m ON m.id = p.message_id
              WHERE p.session_id = ?1
              ORDER BY p.time_created ASC, p.sequence ASC, p.id ASC",
        )
        .map_err(|error| {
            VibexError::storage(
                "local_history_timeline_query_failed",
                "failed to query local history database",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    let rows = statement
        .query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| {
            VibexError::storage(
                "local_history_timeline_query_failed",
                "failed to query local history database",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    let mut timeline = Vec::new();
    for row in rows.flatten() {
        let Ok(message) = serde_json::from_str::<Value>(&row.0) else {
            continue;
        };
        let Ok(part) = serde_json::from_str::<Value>(&row.1) else {
            continue;
        };
        let timestamp = Some(timestamp_number_to_ms(row.2));
        let role = message.get("role").and_then(Value::as_str);
        match (role, string_field(&part, "type").as_deref()) {
            (Some("user"), Some("text")) => {
                if let Some(entry) = user_entry(text_string_or_parts(part.get("text")), timestamp) {
                    timeline.push(entry);
                }
            }
            (Some("assistant" | "model"), Some("text")) => {
                if let Some(entry) = agent_entry(text_string_or_parts(part.get("text")), timestamp)
                {
                    timeline.push(entry);
                }
            }
            (Some("assistant" | "model"), Some("reasoning")) => {
                if let Some(entry) =
                    reasoning_entry(text_string_or_parts(part.get("text")), timestamp)
                {
                    timeline.push(entry);
                }
            }
            (Some("assistant" | "model"), Some("tool")) => {
                timeline.push(tool_entry(
                    string_field(&part, "callID"),
                    string_field(&part, "tool"),
                    json_preview(part.pointer("/state/input")),
                    json_preview(part.pointer("/state/output")),
                    part.pointer("/state/status").and_then(Value::as_str) == Some("error"),
                    timestamp,
                ));
            }
            _ => {}
        }
    }
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

fn scan_cursor(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let chats = root_child(&root.root, "chats");
    let acp = if root.root.file_name().and_then(|name| name.to_str()) == Some("acp-sessions") {
        root.root.clone()
    } else {
        root.root.join("acp-sessions")
    };
    for group in direct_subdirs(&chats) {
        for session_dir in direct_subdirs(&group) {
            append_cursor_summary(&mut batch, &session_dir);
            if batch.found.len() >= MAX_SESSIONS_PER_SOURCE {
                return batch;
            }
        }
    }
    for session_dir in direct_subdirs(&acp) {
        append_cursor_summary(&mut batch, &session_dir);
        if batch.found.len() >= MAX_SESSIONS_PER_SOURCE {
            break;
        }
    }
    batch
}

fn append_cursor_summary(batch: &mut ScanBatch, session_dir: &Path) {
    let database = session_dir.join("store.db");
    if !database.is_file() {
        return;
    }
    let Some(id) = session_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
    else {
        return;
    };
    match parse_cursor_summary(&database, session_dir, &id) {
        Ok(Some(summary)) => batch.found.push(FoundSession {
            summary,
            locator: LocalHistoryLocator::Cursor {
                database,
                session_id: id,
            },
        }),
        Ok(None) => {}
        Err(_) => batch.diagnostics.push(diagnostic(
            LocalHistorySource::Cursor,
            "local_history_database_unreadable",
            "A local history database could not be opened or queried",
        )),
    }
}

fn decode_hex(value: &[u8]) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(value.len() / 2);
    for pair in value.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)? as u8;
        let low = (pair[1] as char).to_digit(16)? as u8;
        out.push((high << 4) | low);
    }
    Some(out)
}

fn encode_hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn cursor_json_bytes(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice(bytes)
        .ok()
        .or_else(|| decode_hex(bytes).and_then(|decoded| serde_json::from_slice(&decoded).ok()))
}

fn cursor_open_store(path: &Path) -> Result<Connection, String> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(path, flags)
        .or_else(|_| {
            Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
        })
        .map_err(|error| error.to_string())?;
    let _ = connection.busy_timeout(std::time::Duration::from_millis(200));
    connection
        .query_row("SELECT count(*) FROM meta", [], |row| row.get::<_, i64>(0))
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

fn cursor_meta(connection: &Connection) -> Result<Value, String> {
    let raw = connection
        .query_row(
            "SELECT value FROM meta WHERE key = '0' LIMIT 1",
            [],
            |row| {
                row.get_ref(0).map(|value| match value {
                    rusqlite::types::ValueRef::Blob(bytes) => bytes.to_vec(),
                    rusqlite::types::ValueRef::Text(text) => text.to_vec(),
                    _ => Vec::new(),
                })
            },
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "local history metadata row is missing".to_string())?;
    cursor_json_bytes(&raw).ok_or_else(|| "local history metadata is invalid".to_string())
}

fn cursor_blob(connection: &Connection, blob_id: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let encoded = encode_hex(blob_id);
    let bytes = connection
        .query_row(
            "SELECT data FROM blobs WHERE id = ?1 LIMIT 1",
            params![encoded],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if bytes.is_some() {
        return Ok(bytes);
    }
    let Some(as_text) = std::str::from_utf8(blob_id).ok() else {
        return Ok(None);
    };
    connection
        .query_row(
            "SELECT data FROM blobs WHERE id = ?1 LIMIT 1",
            params![as_text],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(|error| error.to_string())
}

#[derive(Default)]
struct CursorExtract {
    events: Vec<CursorEvent>,
    used_tokens: Option<u64>,
    max_tokens: Option<u64>,
    workspace: Option<String>,
    git_branch: Option<String>,
    started_at: Option<i64>,
    first_timestamp: Option<i64>,
    last_timestamp: Option<i64>,
}

enum CursorEvent {
    User {
        text: String,
        timestamp: Option<i64>,
        attachments: Vec<MessageAttachment>,
    },
    Agent {
        text: String,
        timestamp: Option<i64>,
    },
    Reasoning {
        text: String,
        timestamp: Option<i64>,
    },
    Tool {
        id: Option<String>,
        name: Option<String>,
        input: Option<String>,
        output: Option<String>,
        failed: bool,
        timestamp: Option<i64>,
    },
}

fn cursor_sidecar(session_dir: &Path) -> Result<Option<Value>, String> {
    let path = session_dir.join("meta.json");
    if !path.is_file() {
        return Ok(None);
    }
    json_file(&path).map(Some)
}

fn parse_cursor_store(database: &Path) -> Result<(Value, CursorExtract), String> {
    let connection = cursor_open_store(database)?;
    let meta = cursor_meta(&connection)?;
    let mut extract = CursorExtract::default();
    let root_id =
        string_field(&meta, "latestRootBlobId").and_then(|value| decode_hex(value.as_bytes()));
    let Some(root_id) = root_id else {
        return Ok((meta, extract));
    };
    let Some(root) = cursor_blob(&connection, &root_id)? else {
        return Ok((meta, extract));
    };
    let mut turn_index = 0usize;
    for (field, value) in proto::fields(&root) {
        match field {
            8 => {
                let current_turn_index = turn_index;
                turn_index = turn_index.saturating_add(1);
                let Some(turn_id) = value.bytes() else {
                    continue;
                };
                let Some(turn) = cursor_blob(&connection, turn_id)? else {
                    continue;
                };
                if let Some(agent_turn) = proto::first_bytes(&turn, 1) {
                    if let Some(user_id) = proto::first_bytes(&agent_turn, 1) {
                        let message = cursor_blob(&connection, &user_id)?.unwrap_or(user_id);
                        let text = cursor_user_text(&connection, &message).unwrap_or_default();
                        let attachments = cursor_user_images(&connection, &message);
                        if !text.trim().is_empty() || !attachments.is_empty() {
                            extract.events.push(CursorEvent::User {
                                text,
                                timestamp: None,
                                attachments,
                            });
                        }
                    }
                    for step_id in proto::messages(&agent_turn, 2) {
                        let step = cursor_blob(&connection, &step_id)?.unwrap_or(step_id);
                        if let Some(message) = proto::first_bytes(&step, 1) {
                            if let Some(text) = proto::first_string(&message, 1)
                                .filter(|text| !text.trim().is_empty())
                            {
                                extract.events.push(CursorEvent::Agent {
                                    text,
                                    timestamp: None,
                                });
                            }
                        }
                        if let Some(message) = proto::first_bytes(&step, 3) {
                            if let Some(text) = proto::first_string(&message, 1)
                                .filter(|text| !text.trim().is_empty())
                            {
                                extract.events.push(CursorEvent::Reasoning {
                                    text,
                                    timestamp: None,
                                });
                            }
                        }
                        if let Some(call) = proto::first_bytes(&step, 2) {
                            if extract.workspace.is_none() && cursor_shell_cwd(&call).is_some() {
                                extract.workspace = cursor_shell_cwd(&call);
                            }
                            extract.events.push(cursor_tool_event(&connection, &call));
                        }
                    }
                } else if let Some(shell_turn) = proto::first_bytes(&turn, 2) {
                    let command = proto::first_bytes(&shell_turn, 1)
                        .and_then(|id| cursor_blob(&connection, &id).ok().flatten().or(Some(id)))
                        .and_then(|message| proto::first_string(&message, 1))
                        .unwrap_or_default();
                    if command.trim().is_empty() {
                        continue;
                    }
                    extract.events.push(CursorEvent::User {
                        text: format!("! {command}"),
                        timestamp: None,
                        attachments: Vec::new(),
                    });
                    let (output, failed) = proto::first_bytes(&shell_turn, 2)
                        .and_then(|id| cursor_blob(&connection, &id).ok().flatten().or(Some(id)))
                        .map(|message| {
                            let stdout = proto::first_string(&message, 1);
                            let stderr = proto::first_string(&message, 2);
                            let exit_code = proto::first_u64(&message, 3).unwrap_or(0);
                            (cursor_join_streams(stdout, stderr), exit_code != 0)
                        })
                        .unwrap_or((None, false));
                    extract.events.push(CursorEvent::Tool {
                        id: Some(format!("cursor-shell-{current_turn_index}")),
                        name: Some("shell".to_string()),
                        input: Some(serde_json::json!({"command": command}).to_string()),
                        output,
                        failed,
                        timestamp: None,
                    });
                }
            }
            5 => {
                if let Some(details) = value.bytes() {
                    extract.used_tokens = proto::first_u64(details, 1);
                    extract.max_tokens = proto::first_u64(details, 2);
                }
            }
            9 => {
                if let Some(uri) = value.string().filter(|value| !value.trim().is_empty()) {
                    extract.workspace =
                        Some(uri.strip_prefix("file://").unwrap_or(uri).to_string());
                }
            }
            14 => {
                if let Some(timing) = value.bytes() {
                    let duration = proto::first_u64(timing, 1).unwrap_or(0);
                    let timestamp = proto::first_u64(timing, 2);
                    extract.first_timestamp = extract
                        .first_timestamp
                        .or(timestamp.map(|value| timestamp_number_to_ms(value as i64)));
                    extract.last_timestamp = timestamp
                        .map(|value| timestamp_number_to_ms(value.saturating_add(duration) as i64));
                }
            }
            21 => {
                if let Some(repo) = value.bytes() {
                    extract.workspace = extract.workspace.or_else(|| proto::first_string(repo, 1));
                    extract.git_branch = proto::first_string(repo, 2);
                }
            }
            26 => {
                extract.started_at = value
                    .u64()
                    .map(|value| timestamp_number_to_ms(value as i64))
            }
            _ => {}
        }
    }
    let turn_timestamp = extract.first_timestamp.or(extract.started_at);
    for event in &mut extract.events {
        match event {
            CursorEvent::User { timestamp, .. }
            | CursorEvent::Agent { timestamp, .. }
            | CursorEvent::Reasoning { timestamp, .. }
            | CursorEvent::Tool { timestamp, .. } => *timestamp = turn_timestamp,
        }
    }
    Ok((meta, extract))
}

fn cursor_user_text(connection: &Connection, message: &[u8]) -> Option<String> {
    proto::first_string(message, 1)
        .or_else(|| {
            proto::first_bytes(message, 18)
                .and_then(|id| cursor_blob(connection, &id).ok().flatten())
                .and_then(|bytes| String::from_utf8(bytes).ok())
        })
        .or_else(|| proto::first_string(message, 8))
}

fn cursor_shell_cwd(call: &[u8]) -> Option<String> {
    let shell_call = proto::first_bytes(call, 1)?;
    let args = proto::first_bytes(&shell_call, 1)?;
    proto::first_string(&args, 2).filter(|value| !value.trim().is_empty())
}

fn cursor_user_images(connection: &Connection, message: &[u8]) -> Vec<MessageAttachment> {
    let mut images = Vec::new();
    for attachment in proto::messages(message, 3) {
        let Some(image) = proto::first_bytes(&attachment, 1) else {
            continue;
        };
        let Some(mime) = proto::first_string(&image, 7).filter(|mime| mime.starts_with("image/"))
        else {
            continue;
        };
        let label = proto::first_string(&image, 2).unwrap_or_else(|| "image".to_string());
        let uri = proto::first_bytes(&image, 1)
            .and_then(|id| cursor_blob(connection, &id).ok().flatten())
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| {
                use base64::{Engine as _, engine::general_purpose::STANDARD};
                format!("data:{mime};base64,{}", STANDARD.encode(bytes))
            });
        images.push(MessageAttachment {
            label,
            mime_type: Some(mime),
            uri,
            inline_text_offset: None,
        });
    }
    images
}

fn cursor_json_object(
    entries: impl IntoIterator<Item = (&'static str, Option<Value>)>,
) -> Option<String> {
    let mut object = Map::new();
    for (key, value) in entries {
        if let Some(value) = value {
            object.insert(key.to_string(), value);
        }
    }
    (!object.is_empty()).then(|| Value::Object(object).to_string())
}

fn cursor_join_streams(stdout: Option<String>, stderr: Option<String>) -> Option<String> {
    let stdout = stdout.unwrap_or_default();
    let stderr = stderr.unwrap_or_default();
    match (stdout.trim(), stderr.trim()) {
        ("", "") => None,
        ("", stderr) => Some(stderr.to_string()),
        (stdout, "") => Some(stdout.to_string()),
        (stdout, stderr) => Some(format!("{stdout}\n{stderr}")),
    }
}

fn cursor_decode_proto_value(bytes: &[u8]) -> Value {
    for (field, value) in proto::fields(bytes) {
        match field {
            1 => return Value::Null,
            2 => {
                if let Some(number) = value.f64().and_then(serde_json::Number::from_f64) {
                    return Value::Number(number);
                }
            }
            3 => {
                if let Some(text) = value.string() {
                    return Value::String(text.to_string());
                }
            }
            4 => {
                if let Some(flag) = value.u64() {
                    return Value::Bool(flag != 0);
                }
            }
            5 => {
                if let Some(structure) = value.bytes() {
                    return cursor_decode_value_map(structure, 1);
                }
            }
            6 => {
                if let Some(items) = value.bytes() {
                    return Value::Array(
                        proto::messages(items, 1)
                            .map(|item| cursor_decode_proto_value(&item))
                            .collect(),
                    );
                }
            }
            _ => {}
        }
    }
    Value::Null
}

fn cursor_decode_value_map(bytes: &[u8], entry_field: u32) -> Value {
    let mut object = Map::new();
    for (field, value) in proto::fields(bytes) {
        if field != entry_field {
            continue;
        }
        let Some(entry) = value.bytes() else { continue };
        let Some(key) = proto::first_string(entry, 1) else {
            continue;
        };
        let value = proto::first_bytes(entry, 2)
            .map(|value| cursor_decode_proto_value(&value))
            .unwrap_or(Value::Null);
        object.insert(key, value);
    }
    Value::Object(object)
}

fn cursor_result_text(result: &[u8]) -> Option<String> {
    proto::first_string(result, 1)
        .or_else(|| proto::first_string(result, 2))
        .or_else(|| proto::first_string(result, 5))
        .or_else(|| proto::first_string(result, 8))
}

fn cursor_tool_event(connection: &Connection, call: &[u8]) -> CursorEvent {
    let id = proto::first_string(call, 57);
    let mut variant = None;
    for (field, value) in proto::fields(call) {
        if field == 57 {
            continue;
        }
        let Some(payload) = value.bytes() else {
            continue;
        };
        variant = Some((field, payload.to_vec()));
        break;
    }
    let Some((field, payload)) = variant else {
        return CursorEvent::Tool {
            id,
            name: None,
            input: None,
            output: None,
            failed: false,
            timestamp: None,
        };
    };
    let mut name = cursor_tool_name(field).unwrap_or("tool").to_string();
    let args = proto::first_bytes(&payload, 1);
    let result = proto::first_bytes(&payload, 2);
    let mut input = None;
    let mut output = None;
    let mut failed = false;
    match field {
        1 => {
            if let Some(args) = args.as_deref() {
                input = cursor_json_object([
                    ("command", proto::first_string(args, 1).map(Value::String)),
                    ("cwd", proto::first_string(args, 2).map(Value::String)),
                    (
                        "description",
                        proto::first_string(args, 15).map(Value::String),
                    ),
                ]);
            }
            if let Some(result) = result.as_deref() {
                for (kind, body) in proto::fields(result) {
                    let Some(body) = body.bytes() else { continue };
                    match kind {
                        1 | 2 => {
                            let interleaved = if kind == 1 { 10 } else { 9 };
                            output = proto::first_string(body, interleaved).or_else(|| {
                                cursor_join_streams(
                                    proto::first_string(body, 5),
                                    proto::first_string(body, 6),
                                )
                            });
                            failed = kind == 2
                                || proto::first_u64(body, 3).is_some_and(|code| code != 0);
                            break;
                        }
                        3 => {
                            output = Some("Command timed out".to_string());
                            failed = true;
                            break;
                        }
                        4 => {
                            output = Some("Command rejected".to_string());
                            failed = true;
                            break;
                        }
                        5 | 7 => {
                            output = proto::first_string(body, 1);
                            failed = true;
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
        3 => {
            input = args.as_deref().and_then(|args| {
                cursor_json_object([("file_path", proto::first_string(args, 1).map(Value::String))])
            });
            if let Some(result) = result.as_deref() {
                output = cursor_result_text(result);
                failed = proto::first_bytes(result, 2).is_some();
            }
        }
        4 => {
            input = args.as_deref().and_then(|args| {
                cursor_json_object([
                    ("path", proto::first_string(args, 1).map(Value::String)),
                    ("pattern", proto::first_string(args, 2).map(Value::String)),
                ])
            });
            if let Some(result) = result.as_deref()
                && let Some(success) = proto::first_bytes(result, 1)
            {
                let files = proto::fields(&success)
                    .into_iter()
                    .filter(|(field, _)| *field == 3)
                    .filter_map(|(_, value)| value.string().map(str::to_string))
                    .collect::<Vec<_>>();
                if !files.is_empty() {
                    output = Some(files.join("\n"));
                }
            }
        }
        5 => {
            input = args.as_deref().and_then(|args| {
                cursor_json_object([
                    ("pattern", proto::first_string(args, 1).map(Value::String)),
                    ("path", proto::first_string(args, 2).map(Value::String)),
                    ("glob", proto::first_string(args, 3).map(Value::String)),
                ])
            });
            output = result.as_deref().and_then(cursor_result_text);
        }
        8 => {
            if let Some(args) = args.as_deref() {
                let mut object = Map::new();
                if let Some(path) = proto::first_string(args, 1) {
                    object.insert("file_path".to_string(), Value::String(path));
                }
                for (key, number) in [("offset", 2), ("limit", 3)] {
                    if let Some(number) = proto::first_u64(args, number) {
                        object.insert(key.to_string(), Value::Number(number.into()));
                    }
                }
                if !object.is_empty() {
                    input = Some(Value::Object(object).to_string());
                }
            }
            if let Some(result) = result.as_deref() {
                if let Some(success) = proto::first_bytes(result, 1) {
                    output = proto::first_string(&success, 1)
                        .or_else(|| {
                            proto::first_bytes(&success, 10)
                                .and_then(|id| cursor_blob(connection, &id).ok().flatten())
                                .and_then(|bytes| String::from_utf8(bytes).ok())
                        })
                        .or_else(|| {
                            (proto::first_bytes(&success, 6).is_some()
                                || proto::first_bytes(&success, 9).is_some())
                            .then(|| "<binary file content>".to_string())
                        });
                } else if let Some(error) = proto::first_bytes(result, 2) {
                    output = cursor_result_text(&error);
                    failed = true;
                }
            }
        }
        9 => {
            if let Some(args) = args.as_deref() {
                let todos = proto::messages(args, 1)
                    .map(|todo| {
                        cursor_json_object([
                            ("content", proto::first_string(&todo, 2).map(Value::String)),
                            ("status", proto::first_string(&todo, 3).map(Value::String)),
                        ])
                        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
                        .unwrap_or(Value::Object(Map::new()))
                    })
                    .collect::<Vec<_>>();
                input = Some(serde_json::json!({"todos": todos}).to_string());
            }
        }
        12 => {
            input = args.as_deref().and_then(|args| {
                cursor_json_object([("file_path", proto::first_string(args, 1).map(Value::String))])
            });
            if let Some(result) = result.as_deref() {
                for (kind, body) in proto::fields(result) {
                    let Some(body) = body.bytes() else { continue };
                    if kind == 1 {
                        output =
                            proto::first_string(body, 5).or_else(|| proto::first_string(body, 8));
                        break;
                    }
                    if (2..=7).contains(&kind) {
                        output = cursor_result_text(body);
                        failed = true;
                        break;
                    }
                }
            }
        }
        13 => {
            input = args.as_deref().and_then(|args| {
                cursor_json_object([("path", proto::first_string(args, 1).map(Value::String))])
            });
            output = result.as_deref().and_then(cursor_result_text);
        }
        15 => {
            if let Some(args) = args.as_deref() {
                let tool = proto::first_string(args, 5).or_else(|| proto::first_string(args, 1));
                let server = proto::first_string(args, 9).or_else(|| proto::first_string(args, 4));
                if let Some(tool) = tool {
                    name = server
                        .map(|server| format!("{server}__{tool}"))
                        .unwrap_or(tool);
                }
                let decoded = cursor_decode_value_map(args, 2);
                if decoded.as_object().is_some_and(|object| !object.is_empty()) {
                    input = Some(decoded.to_string());
                }
            }
            if let Some(result) = result.as_deref() {
                if let Some(success) = proto::first_bytes(result, 1) {
                    let mut texts = Vec::new();
                    for (kind, value) in proto::fields(&success) {
                        if kind == 1 {
                            if let Some(item) = value.bytes() {
                                if let Some(text) = proto::first_string(item, 1) {
                                    texts.push(text);
                                }
                            }
                        } else if kind == 2 {
                            failed = value.u64() == Some(1);
                        }
                    }
                    output = (!texts.is_empty()).then(|| texts.join("\n"));
                } else if let Some(error) = proto::first_bytes(result, 2) {
                    output = cursor_result_text(&error);
                    failed = true;
                }
            }
        }
        16 => {
            output = result
                .as_deref()
                .and_then(|result| proto::first_bytes(result, 1))
                .and_then(|success| proto::first_string(&success, 1));
        }
        17 => {
            input = args.as_deref().and_then(|args| {
                cursor_json_object([("name", proto::first_string(args, 4).map(Value::String))])
            });
            output = args
                .as_deref()
                .and_then(|args| proto::first_string(args, 1));
        }
        18 => {
            input = args.as_deref().and_then(|args| {
                cursor_json_object([("query", proto::first_string(args, 1).map(Value::String))])
            });
            output = result.as_deref().and_then(cursor_result_text);
        }
        19 => {
            if let Some(args) = args.as_deref() {
                let subagent_type = proto::first_bytes(args, 3).and_then(|body| {
                    proto::fields(&body).into_iter().find_map(|(field, _)| {
                        Some(match field {
                            1 => "generalPurpose",
                            2 => "cursor-guide",
                            3 => "best-of-n-runner",
                            _ => "subagent",
                        })
                        .map(str::to_string)
                    })
                });
                input = cursor_json_object([
                    (
                        "description",
                        proto::first_string(args, 1).map(Value::String),
                    ),
                    ("prompt", proto::first_string(args, 2).map(Value::String)),
                    ("subagent_type", subagent_type.map(Value::String)),
                    ("model", proto::first_string(args, 4).map(Value::String)),
                ]);
            }
            if let Some(result) = result.as_deref() {
                if let Some(success) = proto::first_bytes(result, 1) {
                    output = proto::first_string(&success, 1).or_else(|| {
                        proto::first_bytes(&success, 1)
                            .and_then(|body| proto::first_string(&body, 1))
                    });
                } else if let Some(error) = proto::first_bytes(result, 2) {
                    output = cursor_result_text(&error);
                    failed = true;
                }
            }
        }
        23 => {
            if let Some(args) = args.as_deref() {
                let questions = proto::messages(args, 2)
                    .filter_map(|question| proto::first_string(&question, 2).map(Value::String))
                    .collect::<Vec<_>>();
                let mut object = Map::new();
                if let Some(title) = proto::first_string(args, 1) {
                    object.insert("title".to_string(), Value::String(title));
                }
                object.insert("questions".to_string(), Value::Array(questions));
                input = Some(Value::Object(object).to_string());
            }
        }
        24 | 37 => {
            input = args.as_deref().and_then(|args| {
                cursor_json_object([("url", proto::first_string(args, 1).map(Value::String))])
            });
            output = result.as_deref().and_then(cursor_result_text);
        }
        _ => {
            input = args.as_deref().and_then(|args| {
                cursor_json_object([("value", proto::first_string(args, 1).map(Value::String))])
            });
            output = result.as_deref().and_then(cursor_result_text);
        }
    }
    CursorEvent::Tool {
        id,
        name: Some(name),
        input,
        output,
        failed,
        timestamp: None,
    }
}

fn cursor_tool_name(field: u32) -> Option<&'static str> {
    Some(match field {
        1 => "shell",
        3 => "delete",
        4 => "glob",
        5 => "grep",
        8 => "read",
        9 => "update_todos",
        10 => "read_todos",
        12 => "edit",
        13 => "ls",
        14 => "read_lints",
        15 => "mcp",
        16 => "sem_search",
        17 => "create_plan",
        18 => "web_search",
        19 => "task",
        20 => "list_mcp_resources",
        21 => "read_mcp_resource",
        22 => "apply_agent_diff",
        23 => "ask_question",
        24 => "fetch",
        25 => "switch_mode",
        28 => "generate_image",
        29 => "record_screen",
        30 => "computer_use",
        31 => "write_shell_stdin",
        32 => "reflect",
        33 => "setup_vm_environment",
        34 => "truncated_tool_call",
        35 => "start_grind_execution",
        36 => "start_grind_planning",
        37 => "web_fetch",
        38 => "report_bugfix_results",
        39 => "ai_attribution",
        40 => "pr_management",
        41 => "mcp_auth",
        42 => "await",
        43 => "blame_by_file_path",
        44 => "get_mcp_tools",
        45 => "report_bug",
        46 => "set_active_branch",
        48 => "communicate_update",
        49 => "send_final_summary",
        50 => "update_pr_code_tour",
        51 => "replace_env",
        52 => "edit_pr_labels",
        53 => "record_ci_investigation_findings",
        55 => "send_message",
        56 => "fetch_cloud_agent_data",
        58 => "send_to_user",
        61 => "pi_read",
        62 => "pi_bash",
        63 => "pi_edit",
        64 => "pi_write",
        65 => "pi_grep",
        66 => "pi_find",
        67 => "pi_ls",
        68 => "connect_scm",
        69 => "search_conversations",
        _ => return None,
    })
}

fn parse_cursor_summary(
    database: &Path,
    session_dir: &Path,
    session_id: &str,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    let (meta, extract) = parse_cursor_store(database)?;
    if bool_field(&meta, "isSubagent")
        || meta
            .get("subagentInfo")
            .is_some_and(|value| !value.is_null())
    {
        return Ok(None);
    }
    let sidecar = cursor_sidecar(session_dir)?;
    let workspace = sidecar
        .as_ref()
        .and_then(|value| string_field(value, "cwd"))
        .or(extract.workspace.clone())
        .or_else(|| string_field(&meta, "cwd"))
        .or_else(|| string_field(&meta, "workspacePath"));
    let title = string_field(&meta, "name")
        .or_else(|| {
            sidecar
                .as_ref()
                .and_then(|value| string_field(value, "title"))
        })
        .or_else(|| {
            extract.events.iter().find_map(|event| match event {
                CursorEvent::User { text, .. } => Some(title_from_text(text)),
                _ => None,
            })
        });
    let created = meta
        .get("createdAt")
        .and_then(timestamp_value)
        .or(extract.started_at)
        .or(extract.first_timestamp);
    let updated = meta
        .get("updatedAt")
        .and_then(timestamp_value)
        .or(extract.last_timestamp);
    let count = extract
        .events
        .iter()
        .filter(|event| matches!(event, CursorEvent::User { .. } | CursorEvent::Agent { .. }))
        .count() as u32;
    Ok(build_summary(
        LocalHistorySource::Cursor,
        session_id.to_string(),
        title,
        workspace,
        database,
        created,
        updated,
        count,
        string_field(&meta, "lastUsedModel").or_else(|| string_field(&meta, "model")),
    ))
}

fn parse_cursor_timeline(database: &Path) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let (_meta, extract) = parse_cursor_store(database).map_err(local_history_read_error)?;
    let mut timeline = Vec::new();
    for event in extract.events {
        match event {
            CursorEvent::User {
                text,
                timestamp,
                attachments,
            } => {
                if let Some(entry) = user_entry_with_attachments(text, attachments, timestamp) {
                    timeline.push(entry);
                }
            }
            CursorEvent::Agent { text, timestamp } => {
                if let Some(entry) = agent_entry(text, timestamp) {
                    timeline.push(entry);
                }
            }
            CursorEvent::Reasoning { text, timestamp } => {
                if let Some(entry) = reasoning_entry(text, timestamp) {
                    timeline.push(entry);
                }
            }
            CursorEvent::Tool {
                id,
                name,
                input,
                output,
                failed,
                timestamp,
            } => timeline.push(tool_entry(id, name, input, output, failed, timestamp)),
        }
    }
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// Google Antigravity
// ---------------------------------------------------------------------------

fn scan_antigravity(root: &LocalHistorySourceRoot) -> ScanBatch {
    let mut batch = ScanBatch::default();
    let conversations = root_child(&root.root, "conversations");
    for path in direct_files(&conversations) {
        if path.extension().and_then(|extension| extension.to_str()) != Some("db") {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        let metadata = path.with_extension("meta");
        let metadata_path = metadata.is_file().then_some(metadata.clone());
        match parse_antigravity_summary(&path, metadata_path.clone(), &id) {
            Ok(Some(summary)) => batch.found.push(FoundSession {
                summary,
                locator: LocalHistoryLocator::Antigravity {
                    database: path,
                    metadata: metadata_path,
                },
            }),
            Ok(None) => {}
            Err(_) => batch.diagnostics.push(diagnostic(
                LocalHistorySource::Antigravity,
                "local_history_database_unreadable",
                "A local history database could not be opened or queried",
            )),
        }
        if batch.found.len() >= MAX_SESSIONS_PER_SOURCE {
            break;
        }
    }
    batch
}

fn antigravity_steps(database: &Path) -> Result<Vec<Vec<u8>>, String> {
    if fs::metadata(database)
        .map(|metadata| metadata.len() == 0)
        .unwrap_or(true)
    {
        return Ok(Vec::new());
    }
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .or_else(|_| {
        Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    })
    .map_err(|error| error.to_string())?;
    let _ = connection.busy_timeout(std::time::Duration::from_millis(200));
    let mut statement = match connection.prepare("SELECT step_payload FROM steps ORDER BY idx ASC")
    {
        Ok(statement) => statement,
        Err(error) if error.to_string().contains("no such table") => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let rows = statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .map_err(|error| error.to_string())?;
    Ok(rows.flatten().collect())
}

fn antigravity_metadata(path: Option<&Path>) -> Result<Option<Value>, String> {
    path.map(json_file).transpose()
}

fn antigravity_timestamp(step: &[u8]) -> Option<i64> {
    let metadata = proto::first_bytes(step, 5)?;
    let created = proto::first_bytes(&metadata, 1)?;
    let seconds = proto::first_u64(&created, 1)? as i64;
    let nanos = proto::first_u64(&created, 2).unwrap_or(0) as i64;
    Some(
        seconds
            .saturating_mul(1000)
            .saturating_add(nanos / 1_000_000),
    )
}

fn antigravity_completed_timestamp(step: &[u8]) -> Option<i64> {
    let metadata = proto::first_bytes(step, 5)?;
    let completed = proto::first_bytes(&metadata, 8)?;
    let seconds = proto::first_u64(&completed, 1)? as i64;
    let nanos = proto::first_u64(&completed, 2).unwrap_or(0) as i64;
    Some(
        seconds
            .saturating_mul(1000)
            .saturating_add(nanos / 1_000_000),
    )
}

fn antigravity_call_from_bytes(bytes: &[u8]) -> Option<(String, String, Option<String>)> {
    let id = proto::first_string(bytes, 1).filter(|value| !value.trim().is_empty())?;
    let name = proto::first_string(bytes, 2).unwrap_or_else(|| "tool".to_string());
    let args = proto::first_string(bytes, 3).filter(|value| !value.trim().is_empty());
    Some((id, name, args))
}

fn antigravity_call(step: &[u8]) -> Option<(String, String, Option<String>)> {
    let metadata = proto::first_bytes(step, 5)?;
    proto::first_bytes(&metadata, 4).and_then(|bytes| antigravity_call_from_bytes(&bytes))
}

fn antigravity_mcp_call(name: String, args: Option<String>) -> (String, Option<String>) {
    let Some(args) = args else {
        return (name, None);
    };
    let Ok(value) = serde_json::from_str::<Value>(&args) else {
        return (name, Some(args));
    };
    let Some(object) = value.as_object() else {
        return (name, Some(args));
    };
    let Some(tool) = object
        .get("ToolName")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return (name, Some(args));
    };
    let server = object
        .get("ServerName")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let display = server
        .map(|server| format!("{server}_{tool}"))
        .unwrap_or_else(|| tool.to_string());
    let mut input = Map::new();
    if let Some(arguments) = object.get("Arguments") {
        input.insert("arguments".to_string(), arguments.clone());
    }
    for key in [
        "prompt",
        "Description",
        "description",
        "toolAction",
        "toolSummary",
    ] {
        if let Some(prompt) = object
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            input.insert("prompt".to_string(), Value::String(prompt.to_string()));
            break;
        }
    }
    (display, Some(Value::Object(input).to_string()))
}

fn antigravity_tool_name(step: &[u8]) -> Option<&'static str> {
    proto::fields(step).into_iter().find_map(|(field, value)| {
        value.bytes().and_then(|_| match field {
            13 => Some("grep"),
            14 => Some("view_file"),
            15 => Some("list_directory"),
            23 => Some("write_to_file"),
            24 => Some("error"),
            28 => Some("run_command"),
            40 => Some("read_url_content"),
            42 => Some("search_web"),
            47 => Some("mcp"),
            94 => Some("notify_user"),
            98 => Some("file_change"),
            116 => Some("agency_tool_call"),
            _ => None,
        })
    })
}

fn antigravity_tool_outcome(step: &[u8]) -> Option<(Option<String>, bool)> {
    let status_error = proto::first_u64(step, 4) == Some(7);
    for (field, value) in proto::fields(step) {
        let Some(body) = value.bytes() else { continue };
        let outcome = match field {
            13 => {
                let error = proto::first_string(&body, 5);
                let output = error
                    .or_else(|| proto::first_string(&body, 3))
                    .or_else(|| proto::first_string(&body, 1));
                (
                    output,
                    status_error || proto::first_string(&body, 5).is_some(),
                )
            }
            14 => {
                let path = proto::first_string(&body, 1);
                let content = proto::first_string(&body, 4);
                let output = match (path, content) {
                    (Some(path), Some(content)) => Some(format!("{path}\n{content}")),
                    (None, content) => content,
                    (Some(path), None) => Some(path),
                };
                (output, status_error)
            }
            15 => {
                let output = proto::messages(&body, 2)
                    .filter_map(|entry| proto::first_string(&entry, 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                let output = (!output.is_empty())
                    .then_some(output)
                    .or_else(|| proto::first_string(&body, 1));
                (output, status_error)
            }
            23 => {
                let path = proto::first_string(&body, 1).unwrap_or_default();
                let created = proto::first_u64(&body, 4).unwrap_or(0) != 0;
                (
                    Some(format!(
                        "{} {path}",
                        if created { "Created" } else { "Wrote" }
                    )),
                    status_error,
                )
            }
            24 => {
                let nested = proto::first_bytes(&body, 3).unwrap_or(body.to_vec());
                (
                    proto::first_string(&nested, 1).or_else(|| proto::first_string(&nested, 2)),
                    true,
                )
            }
            28 => {
                let command =
                    proto::first_string(&body, 23).or_else(|| proto::first_string(&body, 1));
                let output = proto::first_bytes(&body, 21)
                    .and_then(|combined| {
                        proto::first_string(&combined, 1)
                            .or_else(|| proto::first_string(&combined, 2))
                    })
                    .or_else(|| proto::first_string(&body, 4))
                    .or_else(|| proto::first_string(&body, 5));
                let output = match (command, output) {
                    (Some(command), Some(output)) => Some(format!("$ {command}\n{output}")),
                    (Some(command), None) => Some(format!("$ {command}")),
                    (None, output) => output,
                };
                (
                    output,
                    status_error || proto::first_u64(&body, 6).is_some_and(|code| code != 0),
                )
            }
            40 => (
                proto::first_string(&body, 3).or_else(|| proto::first_string(&body, 1)),
                status_error,
            ),
            42 => {
                let query = proto::first_string(&body, 1);
                let summary = proto::first_string(&body, 5);
                let output = match (query, summary) {
                    (Some(query), Some(summary)) => Some(format!("{query}\n{summary}")),
                    (None, summary) => summary,
                    (Some(query), None) => Some(query),
                };
                (output, status_error)
            }
            47 => {
                let rejected = proto::first_u64(&body, 7).unwrap_or(0) != 0;
                (proto::first_string(&body, 3), status_error || rejected)
            }
            98 => {
                let path = proto::first_string(&body, 1).unwrap_or_default();
                let instruction = proto::first_string(&body, 5).unwrap_or_default();
                (
                    Some(if instruction.is_empty() {
                        format!("Edited {path}")
                    } else {
                        format!("Edited {path}\n{instruction}")
                    }),
                    status_error,
                )
            }
            116 => {
                let output = proto::messages(&body, 4).find_map(|any| {
                    let type_url = proto::first_string(&any, 1)?;
                    if type_url.rsplit('/').next() != Some("antigravity.localharness.ToolResponse")
                    {
                        return None;
                    }
                    let value = proto::first_bytes(&any, 2)?;
                    proto::first_string(&value, 2).or_else(|| proto::first_string(&value, 5))
                });
                (output, status_error)
            }
            _ => continue,
        };
        return Some(outcome);
    }
    None
}

fn antigravity_projection(
    steps: &[Vec<u8>],
) -> (
    Vec<LocalHistoryTimelineEntry>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    u32,
) {
    let mut timeline = Vec::new();
    let mut model = None;
    let mut first = None;
    let mut last = None;
    let mut count = 0u32;
    for step in steps {
        let timestamp = antigravity_timestamp(step);
        first = first.or(timestamp);
        last = antigravity_completed_timestamp(step).or(timestamp).or(last);
        if let Some(info) =
            proto::first_bytes(step, 5).and_then(|metadata| proto::first_bytes(&metadata, 24))
        {
            model = proto::first_string(&info, 8)
                .or_else(|| proto::first_string(&info, 20))
                .or(model);
        }
        if let Some(input) = proto::first_bytes(step, 19) {
            let text = proto::first_string(&input, 1)
                .or_else(|| proto::first_string(&input, 2))
                .or_else(|| {
                    let joined = proto::messages(&input, 3)
                        .filter_map(|item| proto::first_string(&item, 1))
                        .collect::<Vec<_>>()
                        .join("");
                    (!joined.is_empty()).then_some(joined)
                })
                .unwrap_or_default();
            if let Some(entry) = user_entry(text, timestamp) {
                timeline.push(entry);
                count = count.saturating_add(1);
            }
            continue;
        }
        if let Some(planner) = proto::first_bytes(step, 20) {
            if let Some(text) =
                proto::first_string(&planner, 3).filter(|text| !text.trim().is_empty())
            {
                if let Some(entry) = reasoning_entry(text, timestamp) {
                    timeline.push(entry);
                }
            }
            if let Some(text) =
                proto::first_string(&planner, 1).filter(|text| !text.trim().is_empty())
            {
                if let Some(entry) = agent_entry(text, timestamp) {
                    timeline.push(entry);
                    count = count.saturating_add(1);
                }
            }
            for raw_call in proto::messages(&planner, 7) {
                if let Some((id, raw_name, args)) = antigravity_call_from_bytes(&raw_call) {
                    let (name, args) = if raw_name == "call_mcp_tool" {
                        antigravity_mcp_call(raw_name, args)
                    } else {
                        (raw_name, args)
                    };
                    timeline.push(tool_entry(
                        Some(id),
                        Some(name),
                        args,
                        None,
                        false,
                        timestamp,
                    ));
                }
            }
            continue;
        }
        let call = antigravity_call(step);
        let outcome = antigravity_tool_outcome(step);
        if let Some((id, raw_name, raw_args)) = call {
            let (name, input) = if raw_name == "call_mcp_tool" {
                antigravity_mcp_call(raw_name, raw_args)
            } else {
                (raw_name, raw_args)
            };
            let (output, failed) = outcome.unwrap_or((None, proto::first_u64(step, 4) == Some(7)));
            let mut replaced = false;
            for item in timeline.iter_mut().rev() {
                if let TimelinePayload::ToolCall(tool) = &mut item.payload {
                    if tool.tool_call_id == id {
                        tool.input_summary = tool.input_summary.clone().or(input.clone());
                        tool.output_summary = output.clone();
                        tool.status = if failed {
                            ToolCallStatus::Failed
                        } else {
                            ToolCallStatus::Completed
                        };
                        replaced = true;
                        break;
                    }
                }
            }
            if !replaced {
                timeline.push(tool_entry(
                    Some(id),
                    Some(name),
                    input,
                    output,
                    failed,
                    timestamp,
                ));
            }
        } else if let Some((output, failed)) = outcome {
            timeline.push(tool_entry(
                None,
                antigravity_tool_name(step).map(ToOwned::to_owned),
                None,
                output,
                failed,
                timestamp,
            ));
        }
    }
    (timeline, model, first, last, count)
}

fn parse_antigravity_summary(
    database: &Path,
    metadata_path: Option<PathBuf>,
    id: &str,
) -> Result<Option<LocalHistorySessionSummary>, String> {
    let steps = antigravity_steps(database)?;
    let meta = antigravity_metadata(metadata_path.as_deref())?;
    let workspace = meta
        .as_ref()
        .and_then(|value| string_field(value, "cwd").or_else(|| string_field(value, "workspace")));
    let (timeline, parsed_model, started, updated, count) = antigravity_projection(&steps);
    let first_user = timeline.iter().find_map(|entry| match &entry.payload {
        TimelinePayload::UserMessage(message) => Some(title_from_text(&message.text)),
        _ => None,
    });
    let model =
        parsed_model.or_else(|| meta.as_ref().and_then(|value| string_field(value, "model")));
    Ok(build_summary(
        LocalHistorySource::Antigravity,
        id.to_string(),
        first_user,
        workspace,
        database,
        started,
        updated,
        count,
        model,
    ))
}

fn parse_antigravity_timeline(database: &Path) -> VibexResult<Vec<LocalHistoryTimelineEntry>> {
    let steps = antigravity_steps(database).map_err(local_history_read_error)?;
    let (timeline, _, _, _, _) = antigravity_projection(&steps);
    require_timeline(timeline)
}

// ---------------------------------------------------------------------------
// Selection and materialization
// ---------------------------------------------------------------------------

pub fn materialize_local_history(
    selection: &LocalHistorySelection,
) -> VibexResult<LocalHistoryMaterializedSession> {
    let roots = local_history_source_roots();
    materialize_local_history_from(selection, &roots)
}

pub fn materialize_local_history_from(
    selection: &LocalHistorySelection,
    roots: &[LocalHistorySourceRoot],
) -> VibexResult<LocalHistoryMaterializedSession> {
    if selection.external_id.trim().is_empty() {
        return Err(VibexError::validation(
            "local_history_session_not_found",
            "selected local history session is no longer available",
        ));
    }
    let root = roots
        .iter()
        .find(|root| root.source == selection.source)
        .ok_or_else(|| {
            VibexError::validation(
                "local_history_source_unavailable",
                "local history source is unavailable",
            )
        })?;
    let key = LocatorKey {
        source: selection.source,
        root: root.root.clone(),
        external_id: selection.external_id.clone(),
    };
    let locator = locator_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).cloned())
        .or_else(|| {
            // This path is used only when a caller did not come from the
            // picker. It still runs the source-specific scanner and never falls
            // back to an arbitrary recursive file search.
            let batch = scan_source(root.clone());
            batch
                .found
                .into_iter()
                .find(|item| item.summary.key.external_id == selection.external_id)
                .map(|item| item.locator)
        })
        .ok_or_else(|| {
            VibexError::validation(
                "local_history_session_not_found",
                "selected local history session is no longer available",
            )
        })?;

    materialize_with_locator(root, selection, &locator)
}

fn materialize_with_locator(
    root: &LocalHistorySourceRoot,
    selection: &LocalHistorySelection,
    locator: &LocalHistoryLocator,
) -> VibexResult<LocalHistoryMaterializedSession> {
    let source = selection.source;
    let (summary, timeline) = match (source, locator) {
        (LocalHistorySource::Claude, LocalHistoryLocator::Transcript(path)) => {
            let fallback = path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                .map(|name| name.replace('-', "/"));
            (
                parse_claude_summary(path, fallback)
                    .map_err(materialize_error)?
                    .ok_or_else(not_found)?,
                parse_claude_timeline(path)?,
            )
        }
        (LocalHistorySource::Codex, LocalHistoryLocator::Transcript(path)) => {
            let mut summary = parse_codex_summary(path)
                .map_err(materialize_error)?
                .ok_or_else(not_found)?;
            if let Some(title) = load_codex_titles(&root.root).get(&summary.key.external_id) {
                summary.title = title.clone();
            }
            (summary, parse_codex_timeline(path)?)
        }
        (LocalHistorySource::Gemini, LocalHistoryLocator::Transcript(path)) => {
            let document = parse_gemini_document(path).map_err(materialize_error)?;
            let alias_dir = path.parent().and_then(Path::parent).ok_or_else(not_found)?;
            (
                parse_gemini_summary(&root.root, alias_dir, path, &document)
                    .map_err(materialize_error)?
                    .ok_or_else(not_found)?,
                parse_gemini_timeline(path)?,
            )
        }
        (LocalHistorySource::Cline, LocalHistoryLocator::Cline { data_root, task_id }) => {
            let history = json_file(&data_root.join("state").join("taskHistory.json"))
                .map_err(materialize_error)?;
            let entry = history
                .as_array()
                .and_then(|entries| {
                    entries
                        .iter()
                        .find(|entry| string_field(entry, "id").as_deref() == Some(task_id))
                })
                .ok_or_else(not_found)?;
            let transcript = data_root
                .join("tasks")
                .join(task_id)
                .join("api_conversation_history.json");
            (
                parse_cline_summary(data_root, entry, &transcript)
                    .map_err(materialize_error)?
                    .ok_or_else(not_found)?,
                parse_cline_timeline(data_root, task_id)?,
            )
        }
        (
            LocalHistorySource::OpenCode,
            LocalHistoryLocator::OpenCode {
                database,
                session_id,
            },
        ) => (
            parse_opencode_summary(database, session_id)
                .map_err(materialize_error)?
                .ok_or_else(not_found)?,
            parse_opencode_timeline(database, session_id)?,
        ),
        (
            LocalHistorySource::Hermes,
            LocalHistoryLocator::Hermes {
                database,
                session_id,
            },
        ) => (
            parse_hermes_summary(database, session_id)
                .map_err(materialize_error)?
                .ok_or_else(not_found)?,
            parse_hermes_timeline(database, session_id)?,
        ),
        (LocalHistorySource::CodeBuddy, LocalHistoryLocator::Transcript(path)) => (
            parse_codebuddy_summary(path)
                .map_err(materialize_error)?
                .ok_or_else(not_found)?,
            parse_codebuddy_timeline(path)?,
        ),
        (LocalHistorySource::Kimi, LocalHistoryLocator::Transcript(path)) => {
            let session_dir = path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .ok_or_else(not_found)?;
            let index = load_kimi_work_dirs(&root_child(&root.root, "sessions"));
            let session_id = session_dir
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(not_found)?;
            (
                parse_kimi_summary(session_dir, session_id, index.get(session_id).cloned())
                    .map_err(materialize_error)?
                    .ok_or_else(not_found)?,
                parse_kimi_timeline(path)?,
            )
        }
        (LocalHistorySource::Pi, LocalHistoryLocator::Transcript(path)) => (
            parse_pi_summary(path)
                .map_err(materialize_error)?
                .ok_or_else(not_found)?,
            parse_pi_timeline(path)?,
        ),
        (LocalHistorySource::Grok, LocalHistoryLocator::Transcript(path)) => {
            let session_dir = path.parent().ok_or_else(not_found)?;
            let session_id = session_dir
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(not_found)?;
            (
                parse_grok_summary(session_dir, session_id)
                    .map_err(materialize_error)?
                    .ok_or_else(not_found)?,
                parse_grok_timeline(path)?,
            )
        }
        (
            LocalHistorySource::DeepSeek,
            LocalHistoryLocator::DeepSeek {
                session_dir,
                attachments_root,
            },
        ) => {
            let path = if session_dir.join("session.jsonl.zstd").is_file() {
                session_dir.join("session.jsonl.zstd")
            } else {
                session_dir.join("session.jsonl")
            };
            let session_id = session_dir
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(not_found)?;
            (
                parse_deepseek_summary(&path, session_id)
                    .map_err(materialize_error)?
                    .ok_or_else(not_found)?,
                parse_deepseek_timeline(session_dir, attachments_root)?,
            )
        }
        (
            LocalHistorySource::Zcode,
            LocalHistoryLocator::Zcode {
                database,
                session_id,
            },
        ) => (
            parse_zcode_summary(database, session_id)
                .map_err(materialize_error)?
                .ok_or_else(not_found)?,
            parse_zcode_timeline(database, session_id)?,
        ),
        (
            LocalHistorySource::Cursor,
            LocalHistoryLocator::Cursor {
                database,
                session_id,
            },
        ) => (
            parse_cursor_summary(
                database,
                database.parent().ok_or_else(not_found)?,
                session_id,
            )
            .map_err(materialize_error)?
            .ok_or_else(not_found)?,
            parse_cursor_timeline(database)?,
        ),
        (
            LocalHistorySource::Antigravity,
            LocalHistoryLocator::Antigravity { database, metadata },
        ) => (
            parse_antigravity_summary(database, metadata.clone(), &selection.external_id)
                .map_err(materialize_error)?
                .ok_or_else(not_found)?,
            parse_antigravity_timeline(database)?,
        ),
        _ => return Err(not_found()),
    };
    let expected_key: LocalHistoryKey = selection.clone().into();
    if summary.key != expected_key {
        return Err(not_found());
    }
    Ok(LocalHistoryMaterializedSession { summary, timeline })
}

fn materialize_error(error: impl ToString) -> VibexError {
    VibexError::storage(
        "local_history_materialize_failed",
        "failed to read local history",
    )
    .with_diagnostic("error", bounded_text(&error.to_string(), 180))
}

fn not_found() -> VibexError {
    VibexError::validation(
        "local_history_session_not_found",
        "selected local history session is no longer available",
    )
}

/// Build a new Vibex session shell for an imported transcript. The database
/// layer owns insertion and timeline sequence assignment.
pub fn session_shell_for_materialized(
    materialized: &LocalHistoryMaterializedSession,
    project_id: vibex_core::ProjectId,
    workspace_id: vibex_core::WorkspaceId,
    now_ms: i64,
) -> AgentSession {
    AgentSession {
        id: vibex_core::VibexSessionId::new(),
        title: materialized.summary.title.clone(),
        project_id,
        workspace_id,
        workspace_root: materialized
            .summary
            .workspace_root
            .clone()
            .unwrap_or_default(),
        workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
        agent_id: materialized.summary.agent_id.clone(),
        state: AgentSessionState::Idle,
        safety: AgentSessionSafety::workspace_write_ask_on_risk(),
        created_at_ms: materialized.summary.started_at_ms.unwrap_or(now_ms),
        updated_at_ms: materialized.summary.updated_at_ms.unwrap_or(now_ms),
        last_message_at_ms: materialized.summary.updated_at_ms.unwrap_or(now_ms),
        archived_at_ms: None,
        deleted_at_ms: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn claude_layout_is_shallow_and_titles_are_latest() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("projects").join("-work-demo");
        fs::create_dir_all(&project).unwrap();
        let path = project.join("session.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(file, r#"{{"type":"user","sessionId":"s1","timestamp":"2026-01-01T00:00:00Z","message":{{"content":"hello"}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{{"model":"m","content":"ok"}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"custom-title","customTitle":"renamed","timestamp":"2026-01-01T00:00:02Z"}}"#).unwrap();
        let result = scan_local_history_from(
            &[LocalHistorySourceRoot {
                source: LocalHistorySource::Claude,
                root: temp.path().to_path_buf(),
            }],
            &[],
        );
        let session = &result.folders[0].sessions[0].summary;
        assert_eq!(session.title, "renamed");
        assert_eq!(session.workspace_root.as_deref(), Some("/work/demo"));
    }

    #[test]
    fn materialization_uses_the_cached_transcript_locator() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join("sessions").join("2026");
        fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join("rollout-s1.jsonl");
        fs::write(
            &path,
            "{\"type\":\"session_meta\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"payload\":{\"id\":\"s1\",\"cwd\":\"/work\"}}\n{\"type\":\"event_msg\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"payload\":{\"type\":\"user_message\",\"message\":\"hello\"}}\n{\"type\":\"event_msg\",\"timestamp\":\"2026-01-01T00:00:02Z\",\"payload\":{\"type\":\"agent_message\",\"message\":\"hi\"}}\n",
        )
        .unwrap();
        let roots = vec![LocalHistorySourceRoot {
            source: LocalHistorySource::Codex,
            root: temp.path().to_path_buf(),
        }];
        let result = scan_local_history_from(&roots, &[]);
        let selection: LocalHistorySelection =
            result.folders[0].sessions[0].summary.key.clone().into();
        let materialized = materialize_local_history_from(&selection, &roots).unwrap();
        assert_eq!(materialized.timeline.len(), 2);
    }

    #[test]
    fn local_history_root_resolvers_match_agent_environment_precedence() {
        let home = PathBuf::from("/home/demo");
        assert_eq!(
            resolve_gemini_base_dir_from(Some(OsString::from("/sandbox")), Some(home.clone())),
            PathBuf::from("/sandbox/.gemini")
        );
        assert_eq!(
            resolve_gemini_base_dir_from(None, Some(home.clone())),
            PathBuf::from("/home/demo/.gemini")
        );

        assert_eq!(
            resolve_dsh_home_from(Some(OsString::from("~/custom")), Some(home.clone())),
            PathBuf::from("/home/demo/custom")
        );
        assert_eq!(
            resolve_dsh_home_from(Some(OsString::from("   ")), Some(home.clone())),
            PathBuf::from("/home/demo/.dsh")
        );
        assert_eq!(
            resolve_dsh_home_from(Some(OsString::from("~other/custom")), Some(home.clone())),
            PathBuf::from("~other/custom")
        );
        assert_eq!(
            resolve_deepseek_sessions_root_from(
                None,
                Some(OsString::from("~/custom")),
                Some(home.clone()),
            ),
            PathBuf::from("/home/demo/custom/sessions")
        );
        assert_eq!(
            resolve_antigravity_acp_dir_from(Some(OsString::from("~/gemini")), Some(home),),
            PathBuf::from("/home/demo/gemini/antigravity-acp")
        );
    }

    #[test]
    fn hermes_content_decoder_handles_prefixed_parts_and_tool_calls() {
        let raw = format!(
            "{HERMES_CONTENT_JSON_PREFIX}[{{\"type\":\"text\",\"text\":\"hello\"}},{{\"type\":\"image_url\",\"image_url\":{{\"url\":\"data:image/png;base64,AA\"}}}}]"
        );
        assert_eq!(
            content_to_text(Some(&raw)).as_deref(),
            Some("hello\n[image]")
        );
        assert_eq!(
            content_to_text(Some("plain text")).as_deref(),
            Some("plain text")
        );
        assert_eq!(
            content_to_text(Some("\0json:not-json")).as_deref(),
            Some("not-json")
        );

        let calls = parse_hermes_tool_calls(
            r#"[{"id":"call-1","function":{"name":"read","arguments":{"path":"a"}}},{"name":"write","arguments":" {\"ok\":true} "}]"#,
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0.as_deref(), Some("call-1"));
        assert_eq!(calls[0].1, "read");
        assert_eq!(calls[0].2.as_deref(), Some(r#"{"path":"a"}"#));
        assert_eq!(calls[1].1, "write");
        assert_eq!(calls[1].2.as_deref(), Some(r#"{"ok":true}"#));
    }

    #[test]
    fn cline_summary_uses_raw_history_count_and_millisecond_times() {
        let temp = tempfile::tempdir().unwrap();
        let task_id = "1700000000123";
        let task_dir = temp.path().join("tasks").join(task_id);
        fs::create_dir_all(&task_dir).unwrap();
        let transcript = task_dir.join("api_conversation_history.json");
        fs::write(
            &transcript,
            r#"[{"role":"user","ts":1700000000999,"content":"hello"},{"role":"assistant","ts":1700000001999,"content":"ok"},{"role":"tool","ts":1700000002999,"content":"result"}]"#,
        )
        .unwrap();
        let entry = serde_json::json!({
            "id": task_id,
            "ts": 1700000003999_i64,
            "task": "hello",
            "cwdOnTaskInitialization": "/work"
        });
        let summary = parse_cline_summary(temp.path(), &entry, &transcript)
            .unwrap()
            .unwrap();
        assert_eq!(summary.message_count, 3);
        assert_eq!(summary.started_at_ms, Some(1700000000123));
        assert_eq!(summary.updated_at_ms, Some(1700000003999));
    }

    #[test]
    fn damaged_cursor_and_antigravity_databases_report_scan_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let cursor_session = temp.path().join("chats").join("group").join("cursor-1");
        fs::create_dir_all(&cursor_session).unwrap();
        fs::write(cursor_session.join("store.db"), b"not sqlite").unwrap();
        let antigravity_dir = temp.path().join("conversations");
        fs::create_dir_all(&antigravity_dir).unwrap();
        fs::write(antigravity_dir.join("ag-1.db"), b"not sqlite").unwrap();

        let result = scan_local_history_from(
            &[
                LocalHistorySourceRoot {
                    source: LocalHistorySource::Cursor,
                    root: temp.path().to_path_buf(),
                },
                LocalHistorySourceRoot {
                    source: LocalHistorySource::Antigravity,
                    root: temp.path().to_path_buf(),
                },
            ],
            &[],
        );
        assert!(result.folders.is_empty());
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.source == LocalHistorySource::Cursor
                && diagnostic.code == "local_history_database_unreadable"
        }));
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.source == LocalHistorySource::Antigravity
                && diagnostic.code == "local_history_database_unreadable"
        }));
    }

    fn deepseek_test_event(event_type: &str, time: i64, data: Value) -> String {
        serde_json::json!({
            "type": event_type,
            "time": time,
            "data": data,
        })
        .to_string()
    }

    fn deepseek_test_header(cwd: &str) -> String {
        serde_json::json!({
            "type": "session",
            "id": "deepseek-test",
            "createdAt": 1_700_000_000_000_i64,
            "cwd": cwd,
            "delegationDepth": 0,
        })
        .to_string()
    }

    #[test]
    fn deepseek_image_only_sessions_are_importable_and_use_fixture_attachments() {
        let temp = tempfile::tempdir().unwrap();
        let sessions_root = temp.path().join("sessions-root");
        let session_dir = sessions_root.join("bucket").join("deepseek-image");
        fs::create_dir_all(&session_dir).unwrap();

        let digest = "a".repeat(64);
        let image_bytes = b"fixture-image";
        let attachment_root = sessions_root.join("attachments").join("v1");
        let object_dir = attachment_root.join("objects").join(&digest[..2]);
        fs::create_dir_all(&object_dir).unwrap();
        fs::write(object_dir.join(&digest), image_bytes).unwrap();

        let log = [
            deepseek_test_header("/work/image"),
            deepseek_test_event(
                "user/message",
                1_700_000_001_000,
                serde_json::json!({
                    "source": {"kind": "user"},
                    "content": [{
                        "type": "image",
                        "attachment": {
                            "attachmentId": format!("sha256:{digest}"),
                            "mediaType": "image/png",
                            "bytes": image_bytes.len(),
                            "name": "screen.png"
                        }
                    }]
                }),
            ),
        ]
        .join("\n");
        fs::write(session_dir.join("session.jsonl"), log).unwrap();

        let roots = vec![LocalHistorySourceRoot {
            source: LocalHistorySource::DeepSeek,
            root: sessions_root.clone(),
        }];
        let scan = scan_local_history_from(&roots, &[]);
        assert_eq!(scan.total_sessions, 1);
        assert_eq!(scan.importable_count, 1);
        assert_eq!(scan.folders[0].sessions[0].summary.message_count, 1);

        let selection: LocalHistorySelection =
            scan.folders[0].sessions[0].summary.key.clone().into();
        let materialized = materialize_local_history_from(&selection, &roots).unwrap();
        let TimelinePayload::UserMessage(user) = &materialized.timeline[0].payload else {
            panic!("expected user message")
        };
        assert!(user.text.is_empty());
        assert_eq!(user.attachments.len(), 1);
        assert!(
            user.attachments[0]
                .uri
                .as_deref()
                .is_some_and(|uri| uri.starts_with("data:image/png;base64,"))
        );
    }

    #[test]
    fn deepseek_prefers_zstd_and_keeps_complete_prefix_after_a_torn_tail() {
        let temp = tempfile::tempdir().unwrap();
        let session_dir = temp.path().join("bucket").join("deepseek-zstd");
        fs::create_dir_all(&session_dir).unwrap();
        let plain = [
            deepseek_test_header("/plain"),
            deepseek_test_event(
                "user/message",
                1_000,
                serde_json::json!({
                    "source": {"kind": "user"},
                    "content": [{"type": "text", "text": "plain"}]
                }),
            ),
        ]
        .join("\n");
        fs::write(session_dir.join("session.jsonl"), plain).unwrap();
        let mut compressed_log = [
            deepseek_test_header("/compressed"),
            deepseek_test_event(
                "user/message",
                2_000,
                serde_json::json!({
                    "source": {"kind": "user"},
                    "content": [{"type": "text", "text": "compressed"}]
                }),
            ),
        ]
        .join("\n");
        compressed_log.push('\n');
        let first = zstd::stream::encode_all(compressed_log.as_bytes(), 0).unwrap();
        let tail = zstd::stream::encode_all(
            deepseek_test_event("turn/start", 3_000, serde_json::json!({"turn": 2})).as_bytes(),
            0,
        )
        .unwrap();
        let mut bytes = first.clone();
        bytes.extend_from_slice(&tail[..tail.len() / 2]);
        fs::write(session_dir.join("session.jsonl.zstd"), bytes).unwrap();

        let parsed =
            parse_deepseek_summary(&session_dir.join("session.jsonl.zstd"), "deepseek-zstd")
                .unwrap()
                .unwrap();
        assert_eq!(parsed.title, "compressed");
        assert_eq!(parsed.workspace_root.as_deref(), Some("/compressed"));
    }

    #[test]
    fn deepseek_null_compaction_error_is_a_successful_marker() {
        let temp = tempfile::tempdir().unwrap();
        let session_dir = temp.path().join("bucket").join("deepseek-compaction");
        fs::create_dir_all(&session_dir).unwrap();
        let log = [
            deepseek_test_header("/work"),
            deepseek_test_event(
                "compaction/end",
                2_000,
                serde_json::json!({"compactionId": "compact-1", "error": null}),
            ),
        ]
        .join("\n");
        fs::write(session_dir.join("session.jsonl"), log).unwrap();
        let timeline =
            parse_deepseek_timeline(&session_dir, &temp.path().join("attachments").join("v1"))
                .unwrap();
        assert!(matches!(
            &timeline[0].payload,
            TimelinePayload::ToolCall(tool)
                if tool.tool_name == "context_compaction"
                    && tool.status == ToolCallStatus::Completed
        ));
    }

    #[test]
    fn zcode_sqlite_sessions_scan_and_materialize_parts() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("cli").join("db").join("db.sqlite");
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE session (
                    id text primary key,
                    parent_id text,
                    directory text not null,
                    title text not null,
                    time_created integer not null,
                    time_updated integer not null
                );
                CREATE TABLE message (
                    id text primary key,
                    session_id text not null,
                    time_created integer not null,
                    data text not null
                );
                CREATE TABLE part (
                    id text primary key,
                    message_id text not null,
                    session_id text not null,
                    time_created integer not null,
                    sequence integer,
                    data text not null
                );",
            )
            .unwrap();
        let message_data = |role: &str, model: Option<&str>| {
            serde_json::json!({
                "role": role,
                "modelID": model,
            })
            .to_string()
        };
        connection
            .execute(
                "INSERT INTO session (id, parent_id, directory, title, time_created, time_updated)
                 VALUES ('sess_root', NULL, '/work/repo', 'root session', 1700000000000, 1700000003000),
                        ('sess_child', 'sess_root', '/work/repo', 'delegated', 1700000001000, 1700000001500)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO message (id, session_id, time_created, data)
                 VALUES ('m1', 'sess_root', 1700000000000, ?1),
                        ('m2', 'sess_root', 1700000001000, ?2)",
                params![
                    message_data("user", None),
                    message_data("assistant", Some("GLM-5")),
                ],
            )
            .unwrap();
        let parts = vec![
            serde_json::json!({"type": "text", "text": "hello"}),
            serde_json::json!({"type": "reasoning", "text": "thinking"}),
            serde_json::json!({"type": "text", "text": "answer"}),
            serde_json::json!({
                "type": "tool",
                "callID": "call-1",
                "tool": "Bash",
                "state": {
                    "status": "completed",
                    "input": {"command": "ls"},
                    "output": "ok"
                }
            }),
            serde_json::json!({"type": "step-start"}),
        ];
        for (index, data) in parts.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO part (id, message_id, session_id, time_created, sequence, data)
                     VALUES (?1, ?2, 'sess_root', ?3, ?4, ?5)",
                    params![
                        format!("p{index}"),
                        if index == 0 { "m1" } else { "m2" },
                        1700000000000i64 + index as i64 * 100,
                        index as i64,
                        data.to_string(),
                    ],
                )
                .unwrap();
        }

        let roots = vec![LocalHistorySourceRoot {
            source: LocalHistorySource::Zcode,
            root: temp.path().to_path_buf(),
        }];
        let scan = scan_local_history_from(&roots, &[]);
        assert_eq!(scan.total_sessions, 1);
        let summary = &scan.folders[0].sessions[0].summary;
        assert_eq!(summary.title, "root session");
        assert_eq!(summary.workspace_root.as_deref(), Some("/work/repo"));
        assert_eq!(summary.message_count, 2);
        assert_eq!(summary.model.as_deref(), Some("GLM-5"));

        let selection: LocalHistorySelection = summary.key.clone().into();
        let materialized = materialize_local_history_from(&selection, &roots).unwrap();
        assert_eq!(materialized.timeline.len(), 4);
        assert!(matches!(
            &materialized.timeline[0].payload,
            TimelinePayload::UserMessage(_)
        ));
        assert!(matches!(
            &materialized.timeline[1].payload,
            TimelinePayload::Reasoning(_)
        ));
        assert!(matches!(
            &materialized.timeline[2].payload,
            TimelinePayload::AgentMessage(_)
        ));
        let TimelinePayload::ToolCall(tool) = &materialized.timeline[3].payload else {
            panic!("expected tool call");
        };
        assert_eq!(tool.tool_call_id, "call-1");
        assert_eq!(tool.tool_name, "Bash");
        assert_eq!(tool.status, ToolCallStatus::Completed);
    }

    fn append_pb_varint(value: u64, out: &mut Vec<u8>) {
        let mut value = value;
        while value >= 0x80 {
            out.push((value as u8) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    fn pb_varint(field: u32, value: u64, out: &mut Vec<u8>) {
        append_pb_varint((u64::from(field) << 3) | 0, out);
        append_pb_varint(value, out);
    }

    fn pb_bytes(field: u32, value: &[u8], out: &mut Vec<u8>) {
        append_pb_varint((u64::from(field) << 3) | 2, out);
        append_pb_varint(value.len() as u64, out);
        out.extend(value);
    }

    fn pb_string(field: u32, value: &str, out: &mut Vec<u8>) {
        pb_bytes(field, value.as_bytes(), out);
    }

    #[test]
    fn cursor_sqlite_dag_decodes_messages_and_read_tool_results() {
        let temp = tempfile::tempdir().unwrap();
        let session_dir = temp.path().join("cursor-1");
        fs::create_dir_all(&session_dir).unwrap();
        let database = session_dir.join("store.db");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value BLOB NOT NULL)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB NOT NULL)",
                [],
            )
            .unwrap();

        let put_blob = |id: &[u8], data: &[u8]| {
            connection
                .execute(
                    "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
                    params![encode_hex(id), data],
                )
                .unwrap();
        };
        let user_id = b"user-1";
        let mut user = Vec::new();
        pb_string(1, "read the file", &mut user);
        put_blob(user_id, &user);

        let mut args = Vec::new();
        pb_string(1, "src/main.rs", &mut args);
        let mut success = Vec::new();
        pb_string(1, "file contents", &mut success);
        let mut result = Vec::new();
        pb_bytes(1, &success, &mut result);
        let mut read_payload = Vec::new();
        pb_bytes(1, &args, &mut read_payload);
        pb_bytes(2, &result, &mut read_payload);
        let mut call = Vec::new();
        pb_bytes(8, &read_payload, &mut call);
        pb_string(57, "call-1", &mut call);

        let mut assistant = Vec::new();
        pb_string(1, "I checked it.", &mut assistant);
        let mut step = Vec::new();
        pb_bytes(1, &assistant, &mut step);
        pb_bytes(2, &call, &mut step);
        put_blob(b"step-1", &step);

        let mut agent_turn = Vec::new();
        pb_bytes(1, user_id, &mut agent_turn);
        pb_bytes(2, b"step-1", &mut agent_turn);
        let mut turn = Vec::new();
        pb_bytes(1, &agent_turn, &mut turn);
        put_blob(b"turn-1", &turn);

        let mut root = Vec::new();
        pb_bytes(8, b"turn-1", &mut root);
        pb_string(9, "file:///work/cursor", &mut root);
        pb_varint(26, 1_700_000_000_000, &mut root);
        put_blob(b"root-1", &root);
        connection
            .execute(
                "INSERT INTO meta (key, value) VALUES ('0', ?1)",
                params![
                    serde_json::json!({
                        "latestRootBlobId": encode_hex(b"root-1"),
                        "createdAt": 1_700_000_000_000_i64,
                        "lastUsedModel": "cursor-model"
                    })
                    .to_string()
                    .as_bytes()
                ],
            )
            .unwrap();
        drop(connection);

        let summary = parse_cursor_summary(&database, &session_dir, "cursor-1")
            .unwrap()
            .unwrap();
        assert_eq!(summary.workspace_root.as_deref(), Some("/work/cursor"));
        assert_eq!(summary.message_count, 2);
        let timeline = parse_cursor_timeline(&database).unwrap();
        assert_eq!(timeline.len(), 3);
        assert!(matches!(
            &timeline[2].payload,
            TimelinePayload::ToolCall(tool)
                if tool.tool_name == "read"
                    && tool.input_summary.as_deref().is_some_and(|input| input.contains("src/main.rs"))
                    && tool.output_summary.as_deref() == Some("file contents")
                    && tool.status == ToolCallStatus::Completed
        ));
    }
}

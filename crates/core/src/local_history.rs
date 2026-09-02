use serde::{Deserialize, Serialize};

use crate::agent::AgentSession;
use crate::agent_config::AgentId;
use crate::ids::VibexSessionId;
use crate::timeline::{TimelinePayload, TimelineSource};

/// Local agent stores that Vibex can inspect without launching an Agent.
///
/// Values are stable provenance identifiers, not runtime routing identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalHistorySource {
    Claude,
    Codex,
    OpenCode,
    Gemini,
    Cline,
    Hermes,
    CodeBuddy,
    Kimi,
    Pi,
    Grok,
    Cursor,
    DeepSeek,
    Zcode,
    Antigravity,
}

impl LocalHistorySource {
    pub const ALL: [Self; 14] = [
        Self::Claude,
        Self::Codex,
        Self::OpenCode,
        Self::Gemini,
        Self::Cline,
        Self::Hermes,
        Self::CodeBuddy,
        Self::Kimi,
        Self::Pi,
        Self::Grok,
        Self::Cursor,
        Self::DeepSeek,
        Self::Zcode,
        Self::Antigravity,
    ];

    pub const fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Gemini => "gemini",
            Self::Cline => "cline",
            Self::Hermes => "hermes",
            Self::CodeBuddy => "codebuddy",
            Self::Kimi => "kimi",
            Self::Pi => "pi",
            Self::Grok => "grok",
            Self::Cursor => "cursor",
            Self::DeepSeek => "deepseek",
            Self::Zcode => "zcode",
            Self::Antigravity => "antigravity",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Gemini => "Gemini CLI",
            Self::Cline => "Cline",
            Self::Hermes => "Hermes",
            Self::CodeBuddy => "CodeBuddy Code",
            Self::Kimi => "Kimi Code",
            Self::Pi => "Pi",
            Self::Grok => "Grok",
            Self::Cursor => "Cursor",
            Self::DeepSeek => "DeepSeek Harness",
            Self::Zcode => "ZCode",
            Self::Antigravity => "Google Antigravity",
        }
    }

    pub fn agent_id(self) -> AgentId {
        let value = match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Gemini => "gemini",
            Self::Cline => "cline",
            Self::Hermes => "hermes",
            Self::CodeBuddy => "codebuddy-code",
            Self::Kimi => "kimi",
            Self::Pi => "pi",
            Self::Grok => "grok",
            Self::Cursor => "cursor",
            Self::DeepSeek => "deepseek-harness",
            Self::Zcode => "zcode",
            Self::Antigravity => "antigravity",
        };
        AgentId::parse(value).expect("built-in local history Agent ids are valid")
    }
}

impl std::fmt::Display for LocalHistorySource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.key())
    }
}

/// Stable identity for one source-owned local session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalHistoryKey {
    pub source: LocalHistorySource,
    pub external_id: String,
}

impl LocalHistoryKey {
    pub fn is_valid(&self) -> bool {
        !self.external_id.trim().is_empty()
    }
}

/// Lightweight metadata collected while scanning a local Agent history store.
/// The scanner deliberately does not retain transcript payloads in this shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalHistorySessionSummary {
    pub key: LocalHistoryKey,
    pub agent_id: AgentId,
    pub title: String,
    pub workspace_root: Option<String>,
    pub source_path: String,
    pub started_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub message_count: u32,
    pub model: Option<String>,
}

/// A scanned history entry together with its reconciliation state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalHistoryScanSession {
    pub summary: LocalHistorySessionSummary,
    pub status: LocalHistoryImportStatus,
}

/// A workspace group displayed by the local history picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalHistoryScanFolder {
    pub workspace_root: String,
    pub sources: Vec<LocalHistorySource>,
    pub sessions: Vec<LocalHistoryScanSession>,
}

/// A bounded scanner failure. It intentionally carries no raw transcript data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalHistoryScanDiagnostic {
    pub source: LocalHistorySource,
    pub code: String,
    pub message: String,
}

/// Reconciled local history available to the import picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalHistoryScanResult {
    pub folders: Vec<LocalHistoryScanFolder>,
    pub total_sessions: u32,
    pub importable_count: u32,
    pub unassigned_count: u32,
    pub diagnostics: Vec<LocalHistoryScanDiagnostic>,
}

/// Stable user selection sent back from the picker.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalHistorySelection {
    pub source: LocalHistorySource,
    pub external_id: String,
}

impl From<LocalHistoryKey> for LocalHistorySelection {
    fn from(key: LocalHistoryKey) -> Self {
        Self {
            source: key.source,
            external_id: key.external_id,
        }
    }
}

impl From<LocalHistorySelection> for LocalHistoryKey {
    fn from(selection: LocalHistorySelection) -> Self {
        Self {
            source: selection.source,
            external_id: selection.external_id,
        }
    }
}

/// One normalized timeline record materialized only for a selected session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalHistoryTimelineEntry {
    pub source: TimelineSource,
    pub payload: TimelinePayload,
    pub timestamp_ms: Option<i64>,
}

/// Freshly re-read source history ready for one atomic Vibex import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalHistoryMaterializedSession {
    pub summary: LocalHistorySessionSummary,
    pub timeline: Vec<LocalHistoryTimelineEntry>,
}

/// Reconciliation state for a scanned local session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalHistoryImportStatus {
    New,
    Imported,
    Deleted,
}

/// Durable provenance record used to reconcile scanner output without parsing
/// previously imported timelines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalHistoryImportRecord {
    pub key: LocalHistoryKey,
    pub session_id: VibexSessionId,
    pub deleted: bool,
}

/// Result of importing a selected batch from freshly scanned local history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalHistoryImportResult {
    pub sessions: Vec<AgentSession>,
    pub already_imported: u32,
    pub not_found: u32,
    pub failed: u32,
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_history_source_maps_to_stable_product_identity() {
        assert_eq!(LocalHistorySource::Codex.key(), "codex");
        assert_eq!(
            LocalHistorySource::CodeBuddy.agent_id().as_str(),
            "codebuddy-code"
        );
        assert_eq!(
            LocalHistorySource::DeepSeek.agent_id().as_str(),
            "deepseek-harness"
        );
        assert_eq!(LocalHistorySource::Zcode.key(), "zcode");
        assert_eq!(LocalHistorySource::Zcode.label(), "ZCode");
        assert_eq!(LocalHistorySource::Zcode.agent_id().as_str(), "zcode");
        assert_eq!(LocalHistorySource::ALL.len(), 14);
    }

    #[test]
    fn local_history_key_rejects_blank_external_id() {
        let key = LocalHistoryKey {
            source: LocalHistorySource::Claude,
            external_id: "   ".to_string(),
        };
        assert!(!key.is_valid());
    }

    #[test]
    fn selection_round_trips_to_the_durable_key() {
        let key = LocalHistoryKey {
            source: LocalHistorySource::Gemini,
            external_id: "thread-1".to_string(),
        };
        assert_eq!(
            LocalHistoryKey::from(LocalHistorySelection::from(key.clone())),
            key
        );
    }
}

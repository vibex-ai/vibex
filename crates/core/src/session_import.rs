use serde::{Deserialize, Serialize};

use crate::agent::AgentSession;
use crate::agent_config::AgentId;
use crate::ids::{CorrelationId, ProviderProfileId, VibexSessionId};
use crate::provider::{ProviderBindingMetadata, ProviderKind, ProviderSessionConfigState};
use crate::timeline::{TimelinePayload, TimelineRedactionState, TimelineSource};
use crate::workspace::WorkspaceMode;

pub const IMPORT_METADATA_SOURCE: &str = "importSource";
pub const IMPORT_METADATA_NATIVE_HISTORY_IMPORTED: &str = "nativeHistoryImported";
pub const IMPORT_METADATA_NATIVE_HISTORY_IMPORT_VERSION: &str = "nativeHistoryImportVersion";
pub const IMPORT_METADATA_CONTINUATION_STATUS: &str = "importContinuationStatus";
pub const IMPORT_METADATA_CONTINUATION_REASON: &str = "importContinuationReason";
pub const IMPORT_METADATA_CANDIDATE_ID: &str = "importCandidateId";
pub const IMPORT_METADATA_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSessionImportSource {
    Codex,
    Claude,
    Acp,
}

impl ExternalSessionImportSource {
    pub const fn provider_kind(self) -> ProviderKind {
        match self {
            Self::Codex => ProviderKind::Codex,
            Self::Claude => ProviderKind::Claude,
            Self::Acp => ProviderKind::Acp,
        }
    }
}

impl std::fmt::Display for ExternalSessionImportSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Acp => "acp",
        };
        f.write_str(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSessionImportCandidateStatus {
    Importable,
    Partial,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSessionContinuationStatus {
    Resumable,
    ReadOnly,
}

impl ExternalSessionContinuationStatus {
    pub const fn as_metadata_value(self) -> &'static str {
        match self {
            Self::Resumable => "resumable",
            Self::ReadOnly => "read_only",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSessionImportDiagnostic {
    pub code: String,
    pub message: String,
    pub source: ExternalSessionImportSource,
    pub redacted_details: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSessionImportTimelineItem {
    pub source: TimelineSource,
    pub payload: TimelinePayload,
    pub provider_correlation_id: Option<String>,
    pub redaction_state: TimelineRedactionState,
    pub timestamp_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSessionImportCandidate {
    pub candidate_id: String,
    pub source: ExternalSessionImportSource,
    /// Concrete Agent identity used for route selection and UI grouping. This
    /// is intentionally separate from `provider_kind`: several ACP Agents can
    /// coexist behind the same provider kind.
    pub agent_id: AgentId,
    pub provider_kind: ProviderKind,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub workspace_root: String,
    /// Additional ACP workspace roots, in the order reported by the Agent.
    /// The primary `workspace_root` is always kept separate because it owns
    /// the Vibex project/workspace identity.
    #[serde(default)]
    pub additional_workspace_roots: Vec<String>,
    pub workspace_mode: WorkspaceMode,
    pub title: String,
    pub native_session_id: Option<String>,
    pub native_thread_id: Option<String>,
    pub native_resume_token: Option<String>,
    pub continuation_status: ExternalSessionContinuationStatus,
    pub continuation_reason: Option<String>,
    pub updated_at_ms: Option<i64>,
    pub session_config_state: Option<ProviderSessionConfigState>,
    pub status: ExternalSessionImportCandidateStatus,
    /// Set by the ACP scanner when the native session is already represented
    /// by a durable Vibex runtime binding. Re-scans remain safe and can show
    /// provenance without offering a duplicate import.
    #[serde(default)]
    pub already_imported: bool,
    pub redaction_state: TimelineRedactionState,
    /// Legacy offline-import payload. Online ACP imports deliberately ignore
    /// this field and load the native session instead. It remains readable for
    /// parity fixtures and older local data.
    pub timeline_items: Vec<ExternalSessionImportTimelineItem>,
    pub diagnostics: Vec<ExternalSessionImportDiagnostic>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExternalSessionImportCandidateWire {
    candidate_id: String,
    source: ExternalSessionImportSource,
    #[serde(default)]
    agent_id: Option<AgentId>,
    provider_kind: ProviderKind,
    provider_profile_id: Option<ProviderProfileId>,
    workspace_root: String,
    #[serde(default)]
    additional_workspace_roots: Vec<String>,
    workspace_mode: WorkspaceMode,
    title: String,
    native_session_id: Option<String>,
    native_thread_id: Option<String>,
    native_resume_token: Option<String>,
    continuation_status: ExternalSessionContinuationStatus,
    continuation_reason: Option<String>,
    updated_at_ms: Option<i64>,
    session_config_state: Option<ProviderSessionConfigState>,
    status: ExternalSessionImportCandidateStatus,
    #[serde(default)]
    already_imported: bool,
    redaction_state: TimelineRedactionState,
    timeline_items: Vec<ExternalSessionImportTimelineItem>,
    diagnostics: Vec<ExternalSessionImportDiagnostic>,
}

impl<'de> Deserialize<'de> for ExternalSessionImportCandidate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ExternalSessionImportCandidateWire::deserialize(deserializer)?;
        let agent_id = wire
            .agent_id
            .unwrap_or_else(|| default_agent_id_for_import_source(wire.source));
        Ok(Self {
            candidate_id: wire.candidate_id,
            source: wire.source,
            agent_id,
            provider_kind: wire.provider_kind,
            provider_profile_id: wire.provider_profile_id,
            workspace_root: wire.workspace_root,
            additional_workspace_roots: wire.additional_workspace_roots,
            workspace_mode: wire.workspace_mode,
            title: wire.title,
            native_session_id: wire.native_session_id,
            native_thread_id: wire.native_thread_id,
            native_resume_token: wire.native_resume_token,
            continuation_status: wire.continuation_status,
            continuation_reason: wire.continuation_reason,
            updated_at_ms: wire.updated_at_ms,
            session_config_state: wire.session_config_state,
            status: wire.status,
            already_imported: wire.already_imported,
            redaction_state: wire.redaction_state,
            timeline_items: wire.timeline_items,
            diagnostics: wire.diagnostics,
        })
    }
}

fn default_agent_id_for_import_source(source: ExternalSessionImportSource) -> AgentId {
    let value = match source {
        ExternalSessionImportSource::Codex => "codex",
        ExternalSessionImportSource::Claude => "claude",
        ExternalSessionImportSource::Acp => "acp",
    };
    AgentId::parse(value).expect("builtin import Agent id must be valid")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSessionImportPreviewRequest {
    pub sources: Vec<ExternalSessionImportSource>,
    pub workspace_root: Option<String>,
    pub correlation_id: Option<CorrelationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSessionImportPreview {
    pub candidates: Vec<ExternalSessionImportCandidate>,
    pub diagnostics: Vec<ExternalSessionImportDiagnostic>,
    pub correlation_id: Option<CorrelationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSessionImportRequest {
    pub candidates: Vec<ExternalSessionImportCandidate>,
    pub correlation_id: Option<CorrelationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSessionImportedTimelineCount {
    pub session_id: VibexSessionId,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSessionImportResult {
    pub sessions: Vec<AgentSession>,
    pub imported_timeline_counts: Vec<ExternalSessionImportedTimelineCount>,
    pub diagnostics: Vec<ExternalSessionImportDiagnostic>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{
        AgentMessagePayload, TimelinePayload, TimelineRedactionState, TimelineSource,
    };

    #[test]
    fn external_session_import_candidate_serializes_provider_neutral_shape() {
        let candidate = ExternalSessionImportCandidate {
            candidate_id: "codex-thread-1".to_string(),
            source: ExternalSessionImportSource::Codex,
            agent_id: AgentId::parse("codex").unwrap(),
            provider_kind: ProviderKind::Codex,
            provider_profile_id: None,
            workspace_root: "/tmp/vibex-import".to_string(),
            additional_workspace_roots: Vec::new(),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            title: "Imported Codex".to_string(),
            native_session_id: None,
            native_thread_id: Some("thread-1".to_string()),
            native_resume_token: None,
            continuation_status: ExternalSessionContinuationStatus::Resumable,
            continuation_reason: None,
            updated_at_ms: Some(1),
            session_config_state: None,
            status: ExternalSessionImportCandidateStatus::Importable,
            already_imported: false,
            redaction_state: TimelineRedactionState::None,
            timeline_items: vec![ExternalSessionImportTimelineItem {
                source: TimelineSource::Agent,
                payload: TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "hello from imported history".to_string(),
                    is_final: true,
                }),
                provider_correlation_id: Some("event-1".to_string()),
                redaction_state: TimelineRedactionState::None,
                timestamp_ms: Some(1),
            }],
            diagnostics: Vec::new(),
        };

        let json = serde_json::to_value(&candidate).unwrap();
        assert_eq!(json["source"], "codex");
        assert_eq!(json["continuationStatus"], "resumable");
        assert_eq!(json["nativeThreadId"], "thread-1");
        assert_eq!(json["timelineItems"][0]["payload"]["type"], "agent_message");

        let decoded: ExternalSessionImportCandidate = serde_json::from_value(json).unwrap();
        assert_eq!(
            decoded.continuation_status,
            ExternalSessionContinuationStatus::Resumable
        );
        assert_eq!(decoded.source.provider_kind(), ProviderKind::Codex);

        let mut legacy_json = serde_json::to_value(&candidate).unwrap();
        legacy_json
            .as_object_mut()
            .expect("candidate should serialize as an object")
            .remove("agentId");
        let legacy_decoded: ExternalSessionImportCandidate =
            serde_json::from_value(legacy_json).unwrap();
        assert_eq!(legacy_decoded.agent_id, AgentId::parse("codex").unwrap());
    }
}

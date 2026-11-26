//! Cross-instance session clone contracts.
//!
//! These DTOs carry a bounded, single-project snapshot of one or more ACP
//! sessions between runtime authorities. Content-bearing fields (timeline
//! payloads, native transcripts, and provider endpoint metadata) are redacted
//! in [`std::fmt::Debug`] so structured logs and error traces never leak user
//! prompts or provider secrets.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::agent::{AgentSession, AgentSessionSafety, SessionToolSettings};
use crate::agent_config::AgentId;
use crate::error::{VibexError, VibexResult};
use crate::ids::{CorrelationId, ProjectId, ProviderProfileId, VibexSessionId, WorkspaceId};
use crate::runtime::SessionRuntimeSelection;
use crate::timeline::{
    TimelinePayload, TimelineRedactionState, TimelineSource, TurnExecutionAttribution,
};
use crate::workspace::WorkspaceMode;

/// Maximum number of sessions carried in one export/import RPC call.
///
/// Larger selections (for example an entire project) are chunked by the client
/// orchestrator into batches within this limit so individual WebSocket / Relay
/// frames stay bounded.
pub const MAX_CLONE_SESSIONS_PER_BUNDLE: usize = 32;

/// Maximum UTF-8 byte size for a single session's native state artifact.
///
/// When an Agent's native transcript exceeds this threshold, the exporter omits
/// the native artifact and sets [`ExportedCloneSession::native_artifact_omitted`]
/// so the target instance falls back to `ContextBridgeService` on first prompt
/// instead of exhausting transport memory.
pub const MAX_NATIVE_ARTIFACT_BYTES_PER_SESSION: usize = 4 * 1024 * 1024;

/// Maximum timeline entries exported for a single cloned session.
pub const MAX_CLONE_TIMELINE_ITEMS_PER_SESSION: usize = 5_000;

/// Marker recorded on `RuntimeBinding::provider_resume_identity` for an
/// inactive binding seeded from a migrated native session artifact.
pub const CLONED_SESSION_RESUME_IDENTITY: &str = "acp-session-clone-v1";

/// Diagnostic key attached to `session_runtime_selection_uninitialized` when a
/// cloned session carries a pre-mapped [`SessionRuntimeSelection`] hint.
pub const CLONED_PREFERRED_RUNTIME_SELECTION_DIAGNOSTIC: &str = "preferredRuntimeSelectionJson";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloneExportRequest {
    pub session_ids: Vec<VibexSessionId>,
    #[serde(default = "default_true")]
    pub include_native_artifacts: bool,
}

const fn default_true() -> bool {
    true
}

const fn default_workspace_mode() -> WorkspaceMode {
    WorkspaceMode::CurrentCheckout
}

impl SessionCloneExportRequest {
    pub fn validate(&self) -> VibexResult<()> {
        if self.session_ids.is_empty() {
            return Err(VibexError::validation(
                "session_clone_empty_selection",
                "At least one session must be selected for cloning",
            ));
        }
        if self.session_ids.len() > MAX_CLONE_SESSIONS_PER_BUNDLE {
            return Err(VibexError::validation(
                "session_clone_bundle_too_large",
                format!(
                    "A clone bundle may contain at most {MAX_CLONE_SESSIONS_PER_BUNDLE} sessions"
                ),
            ));
        }
        Ok(())
    }
}

/// Non-secret metadata describing the source session's Provider Profile so the
/// client orchestrator can match or recreate an equivalent profile on the
/// target runtime.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneProviderProfileHint {
    pub provider_profile_id: ProviderProfileId,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configured_models: Vec<String>,
}

impl fmt::Debug for CloneProviderProfileHint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CloneProviderProfileHint")
            .field("provider_profile_id", &self.provider_profile_id)
            .field("display_name", &self.display_name)
            .field("has_base_url", &self.base_url.is_some())
            .field("protocol", &self.protocol)
            .field("configured_model_count", &self.configured_models.len())
            .finish()
    }
}

const fn default_redaction_state() -> TimelineRedactionState {
    TimelineRedactionState::None
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClonedTimelineEntry {
    pub source: TimelineSource,
    pub payload: TimelinePayload,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<CorrelationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_correlation_id: Option<String>,
    #[serde(default = "default_redaction_state")]
    pub redaction_state: TimelineRedactionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_attribution: Option<TurnExecutionAttribution>,
}

impl fmt::Debug for ClonedTimelineEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClonedTimelineEntry")
            .field("source", &self.source)
            .field("created_at_ms", &self.created_at_ms)
            .field("redaction_state", &self.redaction_state)
            .field("payload", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeClonedPart {
    pub id: String,
    pub time_created: i64,
    pub time_updated: i64,
    pub data_json: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeClonedMessage {
    pub id: String,
    pub time_created: i64,
    pub time_updated: i64,
    pub data_json: String,
    pub parts: Vec<OpenCodeClonedPart>,
}

/// Bounded native Agent state artifact carried alongside the Vibex timeline so
/// `session/resume` or `session/load` on the target instance can preserve the
/// upstream LLM prompt cache prefix.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum NativeSessionCloneArtifact {
    ClaudeTranscript {
        jsonl_utf8: String,
    },
    CodexRollout {
        /// Relative path below `sessions/` (for example `2026/11/25`).
        relative_date_dir: String,
        rollout_jsonl_utf8: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_title: Option<String>,
    },
    OpenCodeSession {
        slug: String,
        title: String,
        version: String,
        time_created: i64,
        time_updated: i64,
        messages: Vec<OpenCodeClonedMessage>,
    },
}

impl NativeSessionCloneArtifact {
    pub fn approx_byte_len(&self) -> usize {
        match self {
            Self::ClaudeTranscript { jsonl_utf8 } => jsonl_utf8.len(),
            Self::CodexRollout {
                relative_date_dir,
                rollout_jsonl_utf8,
                thread_title,
            } => {
                relative_date_dir.len()
                    + rollout_jsonl_utf8.len()
                    + thread_title.as_ref().map_or(0, String::len)
            }
            Self::OpenCodeSession {
                slug,
                title,
                version,
                messages,
                ..
            } => {
                slug.len()
                    + title.len()
                    + version.len()
                    + messages
                        .iter()
                        .map(|msg| {
                            msg.id.len()
                                + msg.data_json.len()
                                + msg
                                    .parts
                                    .iter()
                                    .map(|part| part.id.len() + part.data_json.len())
                                    .sum::<usize>()
                        })
                        .sum::<usize>()
            }
        }
    }
}

impl fmt::Debug for NativeSessionCloneArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::ClaudeTranscript { .. } => "claude_transcript",
            Self::CodexRollout { .. } => "codex_rollout",
            Self::OpenCodeSession { .. } => "opencode_session",
        };
        formatter
            .debug_struct("NativeSessionCloneArtifact")
            .field("kind", &kind)
            .field("approx_bytes", &self.approx_byte_len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedCloneSession {
    pub source_session_id: VibexSessionId,
    pub title: String,
    pub agent_id: AgentId,
    pub safety: AgentSessionSafety,
    #[serde(default)]
    pub tools: SessionToolSettings,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_message_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_selection: Option<SessionRuntimeSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_hint: Option<CloneProviderProfileHint>,
    pub timeline_items: Vec<ClonedTimelineEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_artifact: Option<NativeSessionCloneArtifact>,
    #[serde(default)]
    pub native_artifact_omitted: bool,
}

impl fmt::Debug for ExportedCloneSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExportedCloneSession")
            .field("source_session_id", &self.source_session_id)
            .field("agent_id", &self.agent_id)
            .field("has_runtime_selection", &self.runtime_selection.is_some())
            .field("has_provider_hint", &self.provider_hint.is_some())
            .field("timeline_item_count", &self.timeline_items.len())
            .field("native_artifact", &self.native_artifact)
            .field("native_artifact_omitted", &self.native_artifact_omitted)
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloneBundle {
    pub source_project_id: ProjectId,
    pub source_workspace_root: String,
    pub sessions: Vec<ExportedCloneSession>,
}

impl fmt::Debug for SessionCloneBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionCloneBundle")
            .field("source_project_id", &self.source_project_id)
            .field("session_count", &self.sessions.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCloneSessionEntry {
    pub session: ExportedCloneSession,
    /// Runtime selection mapped onto the target instance's Provider / AuthContext
    /// IDs. When `None` (for example the user skipped cloning a missing
    /// Provider), the session still imports with its full timeline and prompts
    /// the user to choose a runtime when sending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapped_runtime_selection: Option<SessionRuntimeSelection>,
}

impl fmt::Debug for ImportCloneSessionEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportCloneSessionEntry")
            .field("session", &self.session)
            .field(
                "has_mapped_runtime_selection",
                &self.mapped_runtime_selection.is_some(),
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloneImportRequest {
    pub source_workspace_root: String,
    pub target_workspace_root: String,
    #[serde(default = "default_workspace_mode")]
    pub workspace_mode: WorkspaceMode,
    pub sessions: Vec<ImportCloneSessionEntry>,
}

impl SessionCloneImportRequest {
    pub fn validate(&self) -> VibexResult<()> {
        if self.target_workspace_root.trim().is_empty() {
            return Err(VibexError::validation(
                "session_clone_target_workspace_empty",
                "Target workspace directory must not be empty",
            ));
        }
        if self.sessions.is_empty() {
            return Err(VibexError::validation(
                "session_clone_empty_import",
                "At least one session must be provided for import",
            ));
        }
        if self.sessions.len() > MAX_CLONE_SESSIONS_PER_BUNDLE {
            return Err(VibexError::validation(
                "session_clone_bundle_too_large",
                format!(
                    "A clone import may contain at most {MAX_CLONE_SESSIONS_PER_BUNDLE} sessions"
                ),
            ));
        }
        for entry in &self.sessions {
            if entry.session.timeline_items.len() > MAX_CLONE_TIMELINE_ITEMS_PER_SESSION {
                return Err(VibexError::validation(
                    "session_clone_timeline_too_large",
                    format!(
                        "Cloned session timeline exceeds {MAX_CLONE_TIMELINE_ITEMS_PER_SESSION} entries"
                    ),
                ));
            }
            if let Some(artifact) = entry.session.native_artifact.as_ref()
                && artifact.approx_byte_len() > MAX_NATIVE_ARTIFACT_BYTES_PER_SESSION
            {
                return Err(VibexError::validation(
                    "session_clone_native_artifact_too_large",
                    "Cloned native session artifact exceeds the per-session byte limit",
                ));
            }
        }
        Ok(())
    }
}

impl fmt::Debug for SessionCloneImportRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionCloneImportRequest")
            .field("workspace_mode", &self.workspace_mode)
            .field("session_count", &self.sessions.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedCloneSessionMapping {
    pub source_session_id: VibexSessionId,
    pub target_session: AgentSession,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapped_runtime_selection: Option<SessionRuntimeSelection>,
    pub native_state_restorable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloneImportResult {
    pub target_project_id: ProjectId,
    pub target_workspace_id: WorkspaceId,
    pub target_workspace_root: String,
    pub imported_sessions: Vec<ImportedCloneSessionMapping>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{
        TimelinePayload, TimelineRedactionState, TimelineSource, UserMessagePayload,
    };

    #[test]
    fn debug_impls_redact_sensitive_timeline_and_native_content() {
        let entry = ClonedTimelineEntry {
            source: TimelineSource::User,
            payload: TimelinePayload::UserMessage(UserMessagePayload {
                text: "super-secret-prompt-body".to_string(),
                ..Default::default()
            }),
            created_at_ms: 100,
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
        };
        let artifact = NativeSessionCloneArtifact::ClaudeTranscript {
            jsonl_utf8: "{\"text\":\"secret-native-transcript\"}".to_string(),
        };
        let hint = CloneProviderProfileHint {
            provider_profile_id: ProviderProfileId::parse("provider_1").unwrap(),
            display_name: "Custom Proxy".to_string(),
            base_url: Some("https://private-gateway.internal/v1".to_string()),
            protocol: Some("anthropic".to_string()),
            configured_models: vec!["claude-sonnet-4-5".to_string()],
        };
        assert!(!format!("{entry:?}").contains("super-secret-prompt-body"));
        assert!(!format!("{artifact:?}").contains("secret-native-transcript"));
        assert!(!format!("{hint:?}").contains("private-gateway.internal"));
    }

    #[test]
    fn export_and_import_requests_enforce_bounds() {
        let empty = SessionCloneExportRequest {
            session_ids: Vec::new(),
            include_native_artifacts: true,
        };
        assert_eq!(
            empty.validate().unwrap_err().code,
            "session_clone_empty_selection"
        );

        let too_many = SessionCloneExportRequest {
            session_ids: (0..=MAX_CLONE_SESSIONS_PER_BUNDLE)
                .map(|_| VibexSessionId::new())
                .collect(),
            include_native_artifacts: true,
        };
        assert_eq!(
            too_many.validate().unwrap_err().code,
            "session_clone_bundle_too_large"
        );
    }
}

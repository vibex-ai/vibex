use std::fmt;

use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentSession, FileEntryKind, GitStatusSummary, PermissionRequest, PermissionResponseKind,
    PermissionResponseOption, PermissionRiskCategory, RequestId, TerminalId, TimelineItemId,
    WorkspaceId,
};
use vibex_desktop_model::{
    AgentSidebarRow, FileExplorerRow, TimelineConversationTurn, TimelineRow,
};

use crate::ShellKind;

pub const MIN_TOUCH_TARGET_PX: u16 = 44;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentTone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineCardKind {
    UserMessage,
    AgentMessage,
    Reasoning,
    Plan,
    Tool,
    Command,
    FileOperation,
    Diff,
    Terminal,
    Approval,
    Delegation,
    Error,
    Notice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineCardModel {
    pub item_id: TimelineItemId,
    pub kind: TimelineCardKind,
    pub title: String,
    pub summary: String,
    pub collapsible: bool,
    pub expanded: bool,
    pub tone: ComponentTone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSurfaceModel {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
    pub staged: bool,
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileItemModel {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub name: String,
    pub kind: FileEntryKind,
    pub selected: bool,
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSurfaceModel {
    pub terminal_id: TerminalId,
    pub title: String,
    pub connected: bool,
    pub next_sequence: i64,
    pub has_unread_output: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalSurfaceModel {
    pub request_id: RequestId,
    pub title: String,
    pub details: Vec<(String, String)>,
    pub risk_category: PermissionRiskCategory,
    pub allowed_responses: Vec<PermissionResponseKind>,
    pub response_options: Vec<PermissionResponseOption>,
    pub pending: bool,
    pub presentation: ApprovalPresentation,
    pub high_priority: bool,
    pub touch_target_px: u16,
    pub hover_required: bool,
}

impl fmt::Debug for ApprovalSurfaceModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApprovalSurfaceModel")
            .field("request_id", &self.request_id)
            .field("title_bytes", &self.title.len())
            .field("detail_count", &self.details.len())
            .field("risk_category", &self.risk_category)
            .field("allowed_response_count", &self.allowed_responses.len())
            .field("pending", &self.pending)
            .field("presentation", &self.presentation)
            .field("high_priority", &self.high_priority)
            .field("touch_target_px", &self.touch_target_px)
            .field("hover_required", &self.hover_required)
            .finish()
    }
}

impl ApprovalSurfaceModel {
    pub fn from_request(request: &PermissionRequest, shell: ShellKind) -> Self {
        Self {
            request_id: request.id.clone(),
            title: request.title.clone(),
            details: request
                .details
                .iter()
                .map(|detail| (detail.label.clone(), detail.value.clone()))
                .collect(),
            risk_category: request.risk_category,
            allowed_responses: request.allowed_responses.clone(),
            response_options: request.response_options.clone(),
            pending: request.status == vibex_core::PermissionRequestStatus::Pending,
            presentation: if shell == ShellKind::Compact {
                ApprovalPresentation::Sheet
            } else {
                ApprovalPresentation::ProminentCard
            },
            high_priority: true,
            touch_target_px: MIN_TOUCH_TARGET_PX,
            hover_required: false,
        }
    }

    pub fn is_touch_discoverable(&self) -> bool {
        !self.hover_required && self.touch_target_px >= MIN_TOUCH_TARGET_PX
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPresentation {
    ProminentCard,
    Sheet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEditorStatus {
    Loading,
    Clean,
    Dirty,
    Saving,
    Saved,
    Conflict,
    Disconnected,
    Unsupported,
    TooLarge,
}

#[derive(Clone, PartialEq, Eq)]
pub struct FileConflictComparison {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub base_revision: String,
    pub server_revision: String,
    pub local_content: String,
    pub server_content: String,
}

impl fmt::Debug for FileConflictComparison {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileConflictComparison")
            .field("workspace_id", &self.workspace_id)
            .field("path", &self.path)
            .field("base_revision", &self.base_revision)
            .field("server_revision", &self.server_revision)
            .field("local_bytes", &self.local_content.len())
            .field("server_bytes", &self.server_content.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitConfirmationModel {
    pub workspace_id: WorkspaceId,
    pub message: String,
    pub paths: Vec<String>,
    pub confirmed: bool,
    pub touch_target_px: u16,
    pub hover_required: bool,
}

impl fmt::Debug for GitCommitConfirmationModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitCommitConfirmationModel")
            .field("workspace_id", &self.workspace_id)
            .field("message_bytes", &self.message.len())
            .field("path_count", &self.paths.len())
            .field("confirmed", &self.confirmed)
            .field("touch_target_px", &self.touch_target_px)
            .field("hover_required", &self.hover_required)
            .finish()
    }
}

/// Provider-neutral view projection consumed by both the native GPUI shell and
/// the WASM/Capacitor shell. It contains no socket, runtime, or provider state.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentWorkflowView {
    pub generation: u64,
    pub sessions: Vec<AgentSidebarRow>,
    pub active_session: Option<AgentSession>,
    pub timeline_rows: Vec<TimelineRow>,
    pub conversation_turns: Vec<TimelineConversationTurn>,
    pub approvals: Vec<ApprovalSurfaceModel>,
    pub connection: crate::AgentConnectionState,
}

impl fmt::Debug for AgentWorkflowView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentWorkflowView")
            .field("generation", &self.generation)
            .field("session_count", &self.sessions.len())
            .field("has_active_session", &self.active_session.is_some())
            .field("timeline_row_count", &self.timeline_rows.len())
            .field("turn_count", &self.conversation_turns.len())
            .field("approval_count", &self.approvals.len())
            .field("connection", &self.connection)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct FileWorkflowView {
    pub generation: u64,
    pub rows: Vec<FileExplorerRow>,
    pub search: Vec<vibex_core::FileSearchResult>,
    pub selected_path: Option<String>,
    pub active_file: Option<vibex_core::FileReadResponse>,
    pub editor_content: Option<String>,
    pub editor_base_revision: Option<String>,
    pub status: FileEditorStatus,
    pub conflict: Option<FileConflictComparison>,
}

impl fmt::Debug for FileWorkflowView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileWorkflowView")
            .field("generation", &self.generation)
            .field("row_count", &self.rows.len())
            .field("search_count", &self.search.len())
            .field("selected_path", &self.selected_path)
            .field("has_active_file", &self.active_file.is_some())
            .field(
                "editor_content_bytes",
                &self.editor_content.as_deref().map_or(0, str::len),
            )
            .field(
                "has_editor_base_revision",
                &self.editor_base_revision.is_some(),
            )
            .field("status", &self.status)
            .field("has_conflict", &self.conflict.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitWorkflowView {
    pub generation: u64,
    pub status: Option<GitStatusSummary>,
    pub selected_paths: Vec<String>,
    pub commit_confirmation: Option<GitCommitConfirmationModel>,
    pub last_error: Option<String>,
}

impl fmt::Debug for GitWorkflowView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitWorkflowView")
            .field("generation", &self.generation)
            .field("has_status", &self.status.is_some())
            .field("selected_path_count", &self.selected_paths.len())
            .field(
                "has_commit_confirmation",
                &self.commit_confirmation.is_some(),
            )
            .field("last_error", &self.last_error)
            .finish()
    }
}

impl GitCommitConfirmationModel {
    pub fn confirm(&mut self) {
        self.confirmed = true;
    }

    pub fn is_touch_discoverable(&self) -> bool {
        !self.hover_required && self.touch_target_px >= MIN_TOUCH_TARGET_PX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_encode_touch_discoverability_without_hover() {
        let model = ApprovalSurfaceModel {
            request_id: RequestId::new(),
            title: "Allow write".into(),
            details: vec![("Path".into(), "src/lib.rs".into())],
            risk_category: PermissionRiskCategory::FileWrite,
            allowed_responses: vec![
                PermissionResponseKind::Approve,
                PermissionResponseKind::Deny,
            ],
            response_options: Vec::new(),
            pending: true,
            presentation: ApprovalPresentation::Sheet,
            high_priority: true,
            touch_target_px: MIN_TOUCH_TARGET_PX,
            hover_required: false,
        };
        assert!(model.is_touch_discoverable());
        assert!(model.high_priority);
        assert_eq!(model.presentation, ApprovalPresentation::Sheet);
        assert!(serde_json::to_string(&model).unwrap().len() < 1_024);
    }

    #[test]
    fn file_conflict_debug_never_contains_file_contents() {
        let conflict = FileConflictComparison {
            workspace_id: WorkspaceId::new(),
            path: "src/lib.rs".into(),
            base_revision: "rev-1".into(),
            server_revision: "rev-2".into(),
            local_content: "local secret text".into(),
            server_content: "server secret text".into(),
        };
        let debug = format!("{conflict:?}");
        assert!(!debug.contains("local secret text"));
        assert!(!debug.contains("server secret text"));
        assert!(debug.contains("local_bytes"));
    }

    #[test]
    fn approval_and_commit_debug_never_contains_user_text_or_paths() {
        let approval = ApprovalSurfaceModel {
            request_id: RequestId::new(),
            title: "private approval text".into(),
            details: vec![("secret label".into(), "secret value".into())],
            risk_category: PermissionRiskCategory::FileWrite,
            allowed_responses: vec![PermissionResponseKind::Approve],
            response_options: Vec::new(),
            pending: true,
            presentation: ApprovalPresentation::ProminentCard,
            high_priority: true,
            touch_target_px: MIN_TOUCH_TARGET_PX,
            hover_required: false,
        };
        let commit = GitCommitConfirmationModel {
            workspace_id: WorkspaceId::new(),
            message: "private commit message".into(),
            paths: vec!["private/path.txt".into()],
            confirmed: false,
            touch_target_px: MIN_TOUCH_TARGET_PX,
            hover_required: false,
        };
        let debug = format!("{approval:?} {commit:?}");
        for secret in [
            "private approval text",
            "secret label",
            "secret value",
            "private commit message",
            "private/path.txt",
        ] {
            assert!(!debug.contains(secret), "debug leaked {secret}");
        }
        assert!(debug.contains("title_bytes"));
        assert!(debug.contains("message_bytes"));
    }
}

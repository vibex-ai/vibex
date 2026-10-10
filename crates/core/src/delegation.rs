use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::vibex_use::{
    DelegationBlockedOn, DelegationCompletionPolicy, DelegationContextRef, DelegationOwnershipKind,
    DelegationResultRef, DelegationRuntimeSummary, DelegationTaskPhase, ExecutionOutcome,
};
use crate::{
    AgentDelegationId, AgentId, ProviderProfileId, TimelineItemId, VibexExecutionId, VibexSessionId,
};

/// A durable, product-owned relationship between a parent session and a
/// separately executable child session.
///
/// The historical fields keep their names and wire shape so an older client
/// keeps deserializing the same document. The fields added for Vibex-use are
/// all `#[serde(default)]`, which makes the upgrade additive in both
/// directions, and [`Self::status`] remains the compatibility projection of
/// [`Self::phase`] that the legacy three-tool contract reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDelegation {
    pub id: AgentDelegationId,
    pub parent_session_id: VibexSessionId,
    pub parent_timeline_item_id: Option<TimelineItemId>,
    pub child_session_id: Option<VibexSessionId>,
    pub idempotency_key: String,
    pub title: String,
    /// Bounded user-facing task summary. The complete prompt is represented by
    /// the child session's normal message submission.
    pub task_summary: String,
    pub requested_agent_id: Option<AgentId>,
    pub effective_agent_id: Option<AgentId>,
    /// Compatibility projection. Read [`Self::phase`] for the domain state and
    /// call [`AgentDelegation::phase`] rather than matching on this directly.
    pub status: AgentDelegationStatus,
    pub result_summary: Option<String>,
    pub error_code: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub completed_at_ms: Option<i64>,
    /// Whether ending a turn finishes the task or only awaits the owner.
    #[serde(default)]
    pub completion_policy: DelegationCompletionPolicy,
    /// Explicit domain phase.
    #[serde(default)]
    pub phase: DelegationTaskPhase,
    /// Whether the child session is owned by this task or merely controlled.
    #[serde(default)]
    pub ownership_kind: DelegationOwnershipKind,
    /// Root of the ownership tree this task belongs to. It is preserved across
    /// follow-up tasks so a re-used child cannot reset the delegation depth.
    #[serde(default)]
    pub root_session_id: Option<VibexSessionId>,
    /// The finished task this one continues.
    #[serde(default)]
    pub follows_task_id: Option<AgentDelegationId>,
    /// Bumped on every accepted state change; used for optimistic concurrency.
    #[serde(default)]
    pub revision: u64,
    /// Revision of the session controller that currently owns the child.
    #[serde(default)]
    pub controller_revision: u64,
    /// The execution this task is currently waiting on, if any.
    #[serde(default)]
    pub current_execution_id: Option<VibexExecutionId>,
    /// What the task is blocked on. Independent of the phase.
    #[serde(default)]
    pub blocked_on: Option<DelegationBlockedOn>,
    /// Explicit context windows handed to the child.
    #[serde(default)]
    pub context_refs: Vec<DelegationContextRef>,
    /// What the owner asked the child to demonstrate.
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub requested_runtime: Option<DelegationRuntimeSummary>,
    #[serde(default)]
    pub effective_runtime: Option<DelegationRuntimeSummary>,
    /// Fixed result references, one per settled execution.
    #[serde(default)]
    pub result_refs: Vec<DelegationResultRef>,
    /// When a cancel fence was accepted, before the slow interrupt finished.
    #[serde(default)]
    pub cancellation_requested_at_ms: Option<i64>,
    /// When the task was explicitly accepted or cancelled.
    #[serde(default)]
    pub finished_at_ms: Option<i64>,
    /// Fingerprint of the normalized create payload. A retry with the same key
    /// but a different fingerprint is a conflict, not a silent reuse.
    #[serde(default)]
    pub payload_fingerprint: Option<String>,
}

impl AgentDelegation {
    /// A delegation row in the exact shape the historical single-turn bridge
    /// creates it.
    ///
    /// Every Vibex-use field takes its documented default, so a caller that
    /// predates the task model keeps the behaviour it always had: the child's
    /// first turn completes the task, the child is an owned session, and no
    /// result range has been fixed yet.
    #[allow(clippy::too_many_arguments)]
    pub fn single_turn_legacy(
        parent_session_id: VibexSessionId,
        idempotency_key: impl Into<String>,
        title: impl Into<String>,
        task_summary: impl Into<String>,
        effective_agent_id: Option<AgentId>,
        status: AgentDelegationStatus,
        now_ms: i64,
    ) -> Self {
        Self {
            id: AgentDelegationId::new(),
            parent_session_id,
            parent_timeline_item_id: None,
            child_session_id: None,
            idempotency_key: idempotency_key.into(),
            title: title.into(),
            task_summary: task_summary.into(),
            requested_agent_id: None,
            effective_agent_id,
            status,
            result_summary: None,
            error_code: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            started_at_ms: Some(now_ms),
            completed_at_ms: None,
            completion_policy: DelegationCompletionPolicy::SingleTurnLegacy,
            phase: phase_for_legacy_status(status),
            ownership_kind: DelegationOwnershipKind::OwnedChild,
            root_session_id: None,
            follows_task_id: None,
            revision: 1,
            controller_revision: 0,
            current_execution_id: None,
            blocked_on: None,
            context_refs: Vec::new(),
            acceptance_criteria: Vec::new(),
            requested_runtime: None,
            effective_runtime: None,
            result_refs: Vec::new(),
            cancellation_requested_at_ms: None,
            finished_at_ms: None,
            payload_fingerprint: None,
        }
    }

    pub const fn is_terminal(&self) -> bool {
        self.phase.is_terminal()
    }

    /// The domain phase, re-derived from the compatibility status when a row
    /// predates the phase column.
    pub fn phase(&self) -> DelegationTaskPhase {
        if self.phase == DelegationTaskPhase::default() {
            return phase_for_legacy_status(self.status);
        }
        self.phase
    }

    /// Whether a new execution may be queued into this task's session.
    pub fn accepts_new_execution(&self) -> bool {
        matches!(
            self.phase(),
            DelegationTaskPhase::Queued
                | DelegationTaskPhase::Active
                | DelegationTaskPhase::AwaitingReview
        )
    }

    /// Whether the task has settled on an outcome that no later state may undo.
    pub const fn is_immutable_terminal(&self) -> bool {
        self.phase.is_terminal()
    }
}

/// Maps one compatibility status onto the explicit phase. Used for rows written
/// before the phase column existed and for the single transition helper that
/// keeps both columns consistent.
pub const fn phase_for_legacy_status(status: AgentDelegationStatus) -> DelegationTaskPhase {
    match status {
        AgentDelegationStatus::Queued => DelegationTaskPhase::Queued,
        AgentDelegationStatus::Starting => DelegationTaskPhase::Starting,
        AgentDelegationStatus::Running => DelegationTaskPhase::Active,
        AgentDelegationStatus::NeedsInput => DelegationTaskPhase::Active,
        AgentDelegationStatus::Completed => DelegationTaskPhase::Completed,
        AgentDelegationStatus::Failed => DelegationTaskPhase::Failed,
        AgentDelegationStatus::Cancelled => DelegationTaskPhase::Cancelled,
    }
}

/// Projects one domain phase back onto the compatibility status.
pub const fn legacy_status_for_phase(phase: DelegationTaskPhase) -> AgentDelegationStatus {
    match phase {
        DelegationTaskPhase::Queued => AgentDelegationStatus::Queued,
        DelegationTaskPhase::Starting => AgentDelegationStatus::Starting,
        DelegationTaskPhase::Active => AgentDelegationStatus::Running,
        DelegationTaskPhase::AwaitingReview => AgentDelegationStatus::Running,
        DelegationTaskPhase::Cancelling => AgentDelegationStatus::Running,
        DelegationTaskPhase::Completed => AgentDelegationStatus::Completed,
        DelegationTaskPhase::Failed => AgentDelegationStatus::Failed,
        DelegationTaskPhase::Cancelled => AgentDelegationStatus::Cancelled,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDelegationStatus {
    Queued,
    Starting,
    Running,
    NeedsInput,
    Completed,
    Failed,
    Cancelled,
}

impl AgentDelegationStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Request accepted by the local delegation bridge. The parent session comes
/// from the scoped MCP process, never from an Agent-controlled provider field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAgentDelegationRequest {
    pub parent_session_id: VibexSessionId,
    pub idempotency_key: String,
    pub task: String,
    #[serde(default)]
    pub attachments: Vec<crate::MessageAttachment>,
    pub title: Option<String>,
    pub agent_id: Option<AgentId>,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub mode_id: Option<String>,
    /// Whether ending a turn finishes the task or only awaits the owner.
    ///
    /// A caller that predates the task model keeps the historical single-turn
    /// behaviour; every new Vibex-use delegation asks for `owner_review`.
    #[serde(default)]
    pub completion_policy: DelegationCompletionPolicy,
    /// Whether the task created its child or was granted control of an
    /// existing session.
    #[serde(default)]
    pub ownership_kind: DelegationOwnershipKind,
    /// Explicit context windows handed to the child. The parent transcript is
    /// never copied wholesale.
    #[serde(default)]
    pub context_refs: Vec<DelegationContextRef>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    /// The finished task this one continues. It keeps the same child session
    /// and the same originating root.
    #[serde(default)]
    pub follows_task_id: Option<AgentDelegationId>,
    /// Pre-resolved serve-side session id. It is only ever supplied by a
    /// recovery path that already committed to a specific child.
    #[serde(default)]
    pub existing_session_id: Option<VibexSessionId>,
    /// Complete authority-resolved selection. Legacy callers may omit it.
    #[serde(default)]
    pub runtime_selection: Option<crate::SessionRuntimeSelection>,
    /// Authorized, bounded context rendered by the domain service.
    #[serde(default)]
    pub prompt_context: Option<String>,
    /// Durable operation that accepted this delegation.
    #[serde(default)]
    pub operation_id: Option<crate::VibexOperationId>,
}

impl CreateAgentDelegationRequest {
    /// Fingerprint of everything that changes what this request does.
    ///
    /// Service-generated values such as timestamps are deliberately absent, so
    /// an honest retry of the same intent matches while a changed task, target
    /// or context range does not.
    pub fn payload_fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.task.as_bytes());
        hasher.update([0]);
        hasher.update(serde_json::to_vec(&self.attachments).unwrap_or_default());
        hasher.update([0]);
        hasher.update(self.title.as_deref().unwrap_or_default().as_bytes());
        hasher.update([0]);
        hasher.update(
            self.agent_id
                .as_ref()
                .map(AgentId::as_str)
                .unwrap_or_default()
                .as_bytes(),
        );
        hasher.update([0]);
        hasher.update(
            self.provider_profile_id
                .as_ref()
                .map(ProviderProfileId::as_str)
                .unwrap_or_default()
                .as_bytes(),
        );
        hasher.update([0]);
        hasher.update(self.model.as_deref().unwrap_or_default().as_bytes());
        hasher.update([0]);
        hasher.update(
            self.reasoning_effort
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        hasher.update([0]);
        hasher.update(self.mode_id.as_deref().unwrap_or_default().as_bytes());
        hasher.update([0]);
        hasher.update(self.completion_policy.as_str().as_bytes());
        hasher.update([0]);
        hasher.update(
            serde_json::to_vec(&(
                self.ownership_kind,
                &self.follows_task_id,
                &self.existing_session_id,
            ))
            .unwrap_or_default(),
        );
        hasher.update([0]);
        if let Some(selection) = &self.runtime_selection {
            hasher.update(serde_json::to_vec(selection).unwrap_or_default());
        }
        hasher.update([0]);
        hasher.update(
            self.prompt_context
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        hasher.update([0]);
        for criterion in &self.acceptance_criteria {
            hasher.update(criterion.as_bytes());
            hasher.update([1]);
        }
        for context in &self.context_refs {
            hasher.update(context.session_ref.as_uri().as_bytes());
            hasher.update(context.from_sequence.unwrap_or(i64::MIN).to_le_bytes());
            hasher.update(context.through_sequence.unwrap_or(i64::MIN).to_le_bytes());
            hasher.update([2]);
        }
        hex_lower(&hasher.finalize())
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAgentDelegationRequest {
    pub parent_session_id: VibexSessionId,
    pub delegation_id: AgentDelegationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelAgentDelegationRequest {
    pub parent_session_id: VibexSessionId,
    pub delegation_id: AgentDelegationId,
}

/// The lightweight controller of one session.
///
/// A controller records *who is currently allowed to write* to a session. It is
/// deliberately not a runtime lease: leases answer "why must this process stay
/// alive", while this answers "who may queue the next message".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionController {
    pub session_id: VibexSessionId,
    /// The task that currently owns the write path, when one does.
    #[serde(default)]
    pub owner_task_id: Option<AgentDelegationId>,
    /// The parent session that granted the ownership, when one did.
    #[serde(default)]
    pub owner_parent_session_id: Option<VibexSessionId>,
    /// Bumped whenever the controller changes, including a human takeover.
    pub revision: u64,
    /// Whether the user has taken the session over since the last automated
    /// write. A human takeover is never silently reversed.
    pub human_controlled: bool,
    pub updated_at_ms: i64,
}

/// The terminal classification of one execution, kept next to the task it
/// belongs to so a read never has to re-derive it from a session snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionSettlement {
    pub outcome: ExecutionOutcome,
    pub finished_at_ms: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_delegation() -> AgentDelegation {
        AgentDelegation {
            id: AgentDelegationId::new(),
            parent_session_id: VibexSessionId::new(),
            parent_timeline_item_id: None,
            child_session_id: Some(VibexSessionId::new()),
            idempotency_key: "delegate-review".to_string(),
            title: "Review changes".to_string(),
            task_summary: "Inspect the implementation".to_string(),
            requested_agent_id: None,
            effective_agent_id: None,
            status: AgentDelegationStatus::Completed,
            result_summary: Some("No issues found".to_string()),
            error_code: None,
            created_at_ms: 10,
            updated_at_ms: 20,
            started_at_ms: Some(11),
            completed_at_ms: Some(20),
            completion_policy: DelegationCompletionPolicy::SingleTurnLegacy,
            phase: DelegationTaskPhase::Completed,
            ownership_kind: DelegationOwnershipKind::OwnedChild,
            root_session_id: None,
            follows_task_id: None,
            revision: 3,
            controller_revision: 0,
            current_execution_id: None,
            blocked_on: None,
            context_refs: Vec::new(),
            acceptance_criteria: Vec::new(),
            requested_runtime: None,
            effective_runtime: None,
            result_refs: Vec::new(),
            cancellation_requested_at_ms: None,
            finished_at_ms: Some(20),
            payload_fingerprint: None,
        }
    }

    #[test]
    fn delegation_contract_round_trips_with_stable_field_names() {
        let delegation = sample_delegation();

        let encoded = serde_json::to_value(&delegation).unwrap();

        assert!(encoded.get("parentSessionId").is_some());
        assert!(encoded.get("childSessionId").is_some());
        assert!(encoded.get("parent_session_id").is_none());
        assert_eq!(encoded["status"], "completed");
        assert_eq!(encoded["phase"], "completed");
        assert_eq!(encoded["completionPolicy"], "single_turn_legacy");
        assert_eq!(encoded["ownershipKind"], "owned_child");
        assert_eq!(
            serde_json::from_value::<AgentDelegation>(encoded).unwrap(),
            delegation
        );
        assert!(AgentDelegationStatus::Completed.is_terminal());
        assert!(!AgentDelegationStatus::NeedsInput.is_terminal());
    }

    #[test]
    fn rows_written_before_the_phase_column_still_report_a_phase() {
        let mut encoded = serde_json::to_value(sample_delegation()).unwrap();
        let object = encoded.as_object_mut().unwrap();
        object.remove("phase");
        object.remove("completionPolicy");
        object.remove("ownershipKind");
        object.remove("revision");
        object.insert("status".to_string(), serde_json::json!("needs_input"));
        let delegation: AgentDelegation = serde_json::from_value(encoded).unwrap();
        assert_eq!(delegation.phase(), DelegationTaskPhase::Active);
        assert_eq!(
            delegation.completion_policy,
            DelegationCompletionPolicy::SingleTurnLegacy
        );
        assert_eq!(
            delegation.ownership_kind,
            DelegationOwnershipKind::OwnedChild
        );
        assert!(delegation.accepts_new_execution());
        assert!(!delegation.is_terminal());
    }

    #[test]
    fn phase_and_legacy_status_stay_consistent() {
        for status in [
            AgentDelegationStatus::Queued,
            AgentDelegationStatus::Starting,
            AgentDelegationStatus::Running,
            AgentDelegationStatus::NeedsInput,
            AgentDelegationStatus::Completed,
            AgentDelegationStatus::Failed,
            AgentDelegationStatus::Cancelled,
        ] {
            let phase = phase_for_legacy_status(status);
            let projected = legacy_status_for_phase(phase);
            if status == AgentDelegationStatus::NeedsInput {
                // `NeedsInput` is a blocking fact, not a phase: it projects to
                // `Active` and is carried by `blocked_on` instead.
                assert_eq!(projected, AgentDelegationStatus::Running);
            } else {
                assert_eq!(projected, status);
            }
        }
    }

    #[test]
    fn delegation_fingerprint_distinguishes_target_ownership_and_followed_work() {
        let request: CreateAgentDelegationRequest = serde_json::from_value(serde_json::json!({
            "parentSessionId": VibexSessionId::new(),
            "idempotencyKey": "same-intent",
            "task": "Continue the implementation"
        }))
        .unwrap();
        let fingerprint = request.payload_fingerprint();
        let mut controlled = request.clone();
        controlled.ownership_kind = DelegationOwnershipKind::ControlledExisting;
        assert_ne!(controlled.payload_fingerprint(), fingerprint);
        let mut followed = request.clone();
        followed.follows_task_id = Some(AgentDelegationId::new());
        assert_ne!(followed.payload_fingerprint(), fingerprint);
        let mut target = request.clone();
        target.existing_session_id = Some(VibexSessionId::new());
        assert_ne!(target.payload_fingerprint(), fingerprint);
        let mut recovered = request;
        recovered.operation_id = Some(crate::VibexOperationId::new());
        assert_eq!(recovered.payload_fingerprint(), fingerprint);
    }
}

//! Domain service supplied by the authoritative runtime. Transport gates run
//! before this seam; the caller identity is never taken from request JSON.

use async_trait::async_trait;
use vibex_core::{
    DelegationTaskView, HumanAgentMessageRequest, HumanDelegationRequest, HumanDelegationResult,
    PresentationOutcome, SessionAccessRequest, SessionAccessResult, SessionTreePage,
    SessionTreeRequest, TeamAcknowledgeRequest, TeamPresentationRequest, TeamSnapshot,
    TeamSnapshotRequest, TeamTaskControlRequest, TimelineItem, VibexResult,
};

#[async_trait]
pub trait RemoteTeamService: Send + Sync {
    async fn team_snapshot(&self, request: TeamSnapshotRequest) -> VibexResult<TeamSnapshot>;
    async fn session_tree(&self, request: SessionTreeRequest) -> VibexResult<SessionTreePage>;
    async fn send_message_with_mentions(
        &self,
        request: HumanAgentMessageRequest,
        granted_by: String,
    ) -> VibexResult<Vec<TimelineItem>>;
    async fn set_session_access(
        &self,
        request: SessionAccessRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<SessionAccessResult>;
    async fn delegate_session(
        &self,
        request: HumanDelegationRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<HumanDelegationResult>;
    async fn control_team_task(
        &self,
        request: TeamTaskControlRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<DelegationTaskView>;
    async fn present_team(
        &self,
        request: TeamPresentationRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<PresentationOutcome>;
    async fn acknowledge_team_events(
        &self,
        request: TeamAcknowledgeRequest,
        granted_by: String,
    ) -> VibexResult<usize>;
}

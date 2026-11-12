use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentSessionState, ElicitationRequestStatus, PermissionRequestStatus, TimelineItem,
    TimelineItemKind, TimelineLiveEvent, TimelinePayload, TimelineTurnStart, UserMessageDelivery,
    VibexSessionId,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineTurn {
    pub user_item: Option<TimelineItem>,
    pub response_items: Vec<TimelineItem>,
    pub conclusion_item_id: Option<String>,
    pub pending_permission_ids: Vec<String>,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TimelineModel {
    pub session_id: Option<VibexSessionId>,
    pub items: Vec<TimelineItem>,
    pub authoritative_end_sequence: Option<i64>,
    pub needs_authoritative_refetch: bool,
    /// Where the turn the window opens inside begins, when it opens inside one.
    ///
    /// The window's oldest row is then a row in the middle of that turn, so the
    /// projection cannot date it from its own items; this is the anchor the read
    /// that cut the window carried. `None` when the window opens on a turn
    /// boundary, and when the authority did not report one.
    #[serde(default)]
    pub leading_turn_start: Option<TimelineTurnStart>,
    /// Monotonic counter bumped on every `items` mutation; lets callers cache
    /// derived projections (rows/turns) and invalidate cheaply. Not part of the
    /// serialized contract.
    #[serde(skip)]
    pub revision: u64,
}

impl TimelineModel {
    pub fn replace_authoritative(
        &mut self,
        session_id: VibexSessionId,
        items: impl IntoIterator<Item = TimelineItem>,
    ) {
        self.replace_authoritative_with_leading_turn_start(session_id, items, None);
    }

    /// Installs a window, including where the turn it opens inside begins.
    ///
    /// The anchor is part of the window rather than of the rows: a caller that
    /// replaces the items without one is saying the window opens on a boundary
    /// it can read for itself.
    pub fn replace_authoritative_with_leading_turn_start(
        &mut self,
        session_id: VibexSessionId,
        items: impl IntoIterator<Item = TimelineItem>,
        leading_turn_start: Option<TimelineTurnStart>,
    ) {
        let normalized_items = normalize_items(session_id.clone(), items);
        let content_changed =
            self.session_id.as_ref() != Some(&session_id) || self.items != normalized_items;
        self.session_id = Some(session_id);
        if content_changed {
            self.items = normalized_items;
        }
        self.authoritative_end_sequence = self.items.last().map(|item| item.sequence);
        self.needs_authoritative_refetch = false;
        self.leading_turn_start = leading_turn_start;
        if content_changed {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn apply_live(&mut self, event: TimelineLiveEvent) -> bool {
        self.apply_live_batch(std::iter::once(event)) > 0
    }

    pub fn apply_live_batch(
        &mut self,
        events: impl IntoIterator<Item = TimelineLiveEvent>,
    ) -> usize {
        let mut changed = 0;
        for event in events {
            if self.session_id.as_ref() != Some(&event.session_id)
                || event.sequence != event.item.sequence
                || event.item.session_id != event.session_id
            {
                continue;
            }
            if self
                .authoritative_end_sequence
                .is_some_and(|end| event.sequence > end.saturating_add(1))
            {
                self.needs_authoritative_refetch = true;
            }

            if self
                .items
                .last()
                .is_none_or(|last| event.sequence > last.sequence)
            {
                self.items.push(event.item);
                self.authoritative_end_sequence = Some(event.sequence);
                changed += 1;
                continue;
            }

            match self
                .items
                .binary_search_by_key(&event.sequence, |item| item.sequence)
            {
                Ok(index) if self.items[index] != event.item => {
                    self.items[index] = event.item;
                    changed += 1;
                }
                Ok(_) => {}
                Err(index) => {
                    self.items.insert(index, event.item);
                    changed += 1;
                }
            }
            self.authoritative_end_sequence = self.items.last().map(|item| item.sequence);
        }
        if changed > 0 {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    pub fn mark_lagged(&mut self) {
        self.needs_authoritative_refetch = true;
    }

    /// The last sequence in the unbroken run that starts at the window's
    /// first item.
    ///
    /// A gap can arrive before the full refetch does. Resuming from the last
    /// item would then skip the hole, because the gapped item is already at
    /// the end; resuming from the contiguous run asks for exactly what is
    /// missing.
    pub fn contiguous_end_sequence(&self) -> Option<i64> {
        let mut items = self.items.iter();
        let mut end = items.next()?.sequence;
        for item in items {
            if item.sequence != end.saturating_add(1) {
                break;
            }
            end = item.sequence;
        }
        Some(end)
    }

    /// Whether the window has no holes between adjacent items.
    pub fn is_contiguous(&self) -> bool {
        self.items
            .windows(2)
            .all(|pair| pair[1].sequence == pair[0].sequence.saturating_add(1))
    }

    pub fn clear_authoritative_refetch(&mut self) {
        self.needs_authoritative_refetch = false;
    }

    pub fn rows(&self) -> Vec<crate::TimelineRow> {
        crate::timeline_rows(&self.items)
    }

    pub fn turns(&self) -> Vec<TimelineTurn> {
        timeline_turns(&self.items)
    }

    pub fn conversation_turns(
        &self,
        session_state: Option<AgentSessionState>,
        pending_turn_active: bool,
    ) -> Vec<crate::TimelineConversationTurn> {
        crate::timeline_conversation_turns(&self.items, session_state, pending_turn_active)
    }

    pub fn conversation_turns_with_reasoning_mode(
        &self,
        session_state: Option<AgentSessionState>,
        pending_turn_active: bool,
        reasoning_display_mode: crate::ReasoningDisplayMode,
    ) -> Vec<crate::TimelineConversationTurn> {
        crate::timeline_conversation_turns_with_leading_turn_start(
            &self.items,
            session_state,
            pending_turn_active,
            reasoning_display_mode,
            self.leading_turn_start,
        )
    }

    pub fn pending_permission_ids(&self) -> BTreeSet<String> {
        let mut pending = BTreeSet::new();
        for item in &self.items {
            match &item.payload {
                TimelinePayload::PermissionRequest(request)
                    if request.status == PermissionRequestStatus::Pending =>
                {
                    pending.insert(request.id.to_string());
                }
                TimelinePayload::PermissionResolution(resolution) => {
                    pending.remove(resolution.request_id.as_str());
                }
                TimelinePayload::ElicitationRequest(request)
                    if request.status == ElicitationRequestStatus::Pending =>
                {
                    pending.insert(request.id.to_string());
                }
                TimelinePayload::ElicitationResolution(resolution) => {
                    pending.remove(resolution.request_id.as_str());
                }
                _ => {}
            }
        }
        pending
    }
}

pub fn timeline_turns(items: &[TimelineItem]) -> Vec<TimelineTurn> {
    timeline_turn_refs(items.iter())
        .into_iter()
        .map(Into::into)
        .collect()
}

/// The user-input requests the live turn is still waiting on.
///
/// This is the question a sidebar row asks — is the Agent parked on me right
/// now — so it is scoped to the turn the session is in, not to the whole
/// conversation. A request a finished turn left unanswered is not news once a
/// later prompt has claimed the session: an interrupt in particular cancels the
/// native callback without appending a resolution, so the request item stays
/// `Pending` in the transcript forever. Re-counting it from the whole timeline
/// is what kept a row pinned to a card no surface could answer.
///
/// A `Steer` message joins the turn that was already running, so it does not
/// open a new one; every other user message does. A window that opens
/// mid-conversation without a turn boundary keeps every request it holds, since
/// there is no later prompt in it to supersede them.
pub fn live_turn_pending_permission_ids(items: &[TimelineItem]) -> BTreeSet<String> {
    let start = items
        .iter()
        .rposition(|item| {
            matches!(
                &item.payload,
                TimelinePayload::UserMessage(message)
                    if message.delivery != UserMessageDelivery::Steer
            )
        })
        .unwrap_or(0);
    let mut pending = BTreeSet::new();
    for item in &items[start..] {
        match &item.payload {
            TimelinePayload::PermissionRequest(request)
                if request.status == PermissionRequestStatus::Pending =>
            {
                pending.insert(request.id.to_string());
            }
            TimelinePayload::PermissionResolution(resolution) => {
                pending.remove(resolution.request_id.as_str());
            }
            TimelinePayload::ElicitationRequest(request)
                if request.status == ElicitationRequestStatus::Pending =>
            {
                pending.insert(request.id.to_string());
            }
            TimelinePayload::ElicitationResolution(resolution) => {
                pending.remove(resolution.request_id.as_str());
            }
            _ => {}
        }
    }
    pending
}

pub(crate) struct TimelineTurnRef<'a> {
    pub(crate) user_item: Option<&'a TimelineItem>,
    pub(crate) response_items: Vec<&'a TimelineItem>,
    pub(crate) conclusion_item_id: Option<String>,
    pub(crate) pending_permission_ids: Vec<String>,
    pub(crate) failed: bool,
}

impl From<TimelineTurnRef<'_>> for TimelineTurn {
    fn from(turn: TimelineTurnRef<'_>) -> Self {
        Self {
            user_item: turn.user_item.cloned(),
            response_items: turn.response_items.into_iter().cloned().collect(),
            conclusion_item_id: turn.conclusion_item_id,
            pending_permission_ids: turn.pending_permission_ids,
            failed: turn.failed,
        }
    }
}

/// Groups timeline items without cloning their payloads. Presentation builders
/// call this on every streaming update, while the public `timeline_turns`
/// adapter above preserves the existing owned/serializable contract.
pub(crate) fn timeline_turn_refs<'a>(
    items: impl IntoIterator<Item = &'a TimelineItem>,
) -> Vec<TimelineTurnRef<'a>> {
    let mut turns = Vec::new();
    let mut current = empty_turn_ref();
    for item in items {
        if item.kind == TimelineItemKind::UserMessage {
            if current.user_item.is_some() || !current.response_items.is_empty() {
                finish_turn_ref(&mut current);
                turns.push(current);
                current = empty_turn_ref();
            }
            current.user_item = Some(item);
        } else {
            if current.failed
                && item.kind != TimelineItemKind::Error
                && matches!(
                    item.source,
                    vibex_core::TimelineSource::Agent | vibex_core::TimelineSource::Provider
                )
            {
                finish_turn_ref(&mut current);
                turns.push(current);
                current = empty_turn_ref();
            }
            if is_turn_boundary_error(item) {
                current.failed = true;
            }
            current.response_items.push(item);
        }
    }
    if current.user_item.is_some() || !current.response_items.is_empty() {
        finish_turn_ref(&mut current);
        turns.push(current);
    }
    turns
}

fn empty_turn_ref<'a>() -> TimelineTurnRef<'a> {
    TimelineTurnRef {
        user_item: None,
        response_items: Vec::new(),
        conclusion_item_id: None,
        pending_permission_ids: Vec::new(),
        failed: false,
    }
}

fn normalize_items(
    session_id: VibexSessionId,
    items: impl IntoIterator<Item = TimelineItem>,
) -> Vec<TimelineItem> {
    let mut by_sequence = BTreeMap::new();
    for item in items {
        if item.session_id == session_id && item.sequence >= 0 {
            by_sequence.insert(item.sequence, item);
        }
    }
    by_sequence.into_values().collect()
}

fn finish_turn_ref(turn: &mut TimelineTurnRef<'_>) {
    let mut pending_permission_ids = BTreeSet::new();
    for item in &turn.response_items {
        match &item.payload {
            TimelinePayload::PermissionRequest(request)
                if request.status == PermissionRequestStatus::Pending =>
            {
                pending_permission_ids.insert(request.id.to_string());
            }
            TimelinePayload::PermissionResolution(resolution) => {
                pending_permission_ids.remove(resolution.request_id.as_str());
            }
            TimelinePayload::ElicitationRequest(request)
                if request.status == ElicitationRequestStatus::Pending =>
            {
                pending_permission_ids.insert(request.id.to_string());
            }
            TimelinePayload::ElicitationResolution(resolution) => {
                pending_permission_ids.remove(resolution.request_id.as_str());
            }
            _ => {}
        }
    }
    turn.pending_permission_ids = pending_permission_ids.into_iter().collect();
    turn.failed = turn
        .response_items
        .iter()
        .any(|item| is_turn_boundary_error(item));
    // A final answer is the turn's conclusion even when a boundary error
    // followed it. The error explains why the turn stopped, but it is not the
    // thing the turn produced: naming the error here dropped the answer from the
    // turn entirely, because every final agent row is removed from the process
    // rows below. A turn that answered and then failed then showed only the
    // failure, and the reader could not scroll back to what was said. The error
    // stays in the process rows and is read with the rest of the work; the
    // answer stays the conclusion.
    turn.conclusion_item_id = turn
        .response_items
        .iter()
        .rev()
        .find(|item| is_final_agent_message(item))
        .or_else(|| {
            turn.response_items
                .iter()
                .rev()
                .find(|item| is_turn_boundary_error(item))
        })
        .map(|item| item.id.to_string());
}

fn is_turn_boundary_error(item: &TimelineItem) -> bool {
    item.kind == TimelineItemKind::Error && item.provider_correlation_id.is_none()
}

fn is_final_agent_message(item: &TimelineItem) -> bool {
    matches!(
        &item.payload,
        TimelinePayload::AgentMessage(message) if message.is_final
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentMessagePayload, TimelineItemId, TimelineRedactionState, TimelineSource,
        UserMessagePayload,
    };

    fn item(session_id: &VibexSessionId, sequence: i64, payload: TimelinePayload) -> TimelineItem {
        TimelineItem {
            id: TimelineItemId::parse(format!("timeline_{sequence}")).unwrap(),
            session_id: session_id.clone(),
            sequence,
            timestamp_ms: sequence,
            source: if matches!(payload, TimelinePayload::UserMessage(_)) {
                TimelineSource::User
            } else {
                TimelineSource::Agent
            },
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload_depth: vibex_core::TimelinePayloadDepth::Detail,
            payload,
        }
    }

    #[test]
    fn authoritative_merge_is_ordered_idempotent_and_session_fenced() {
        let session = VibexSessionId::parse("session_current").unwrap();
        let other = VibexSessionId::parse("session_other").unwrap();
        let final_message = TimelinePayload::AgentMessage(AgentMessagePayload {
            text: "done".into(),
            is_final: true,
        });
        let mut model = TimelineModel::default();
        model.replace_authoritative(
            session.clone(),
            [
                item(&session, 2, final_message.clone()),
                item(
                    &session,
                    1,
                    TimelinePayload::UserMessage(UserMessagePayload {
                        text: "go".into(),
                        attachments: Vec::new(),
                        ..Default::default()
                    }),
                ),
                item(&other, 3, final_message),
            ],
        );
        assert_eq!(
            model
                .items
                .iter()
                .map(|item| item.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            model.turns()[0].conclusion_item_id.as_deref(),
            Some("timeline_2")
        );
    }

    #[test]
    fn live_batch_keeps_order_and_bumps_revision_once() {
        let session = VibexSessionId::parse("session_batch").unwrap();
        let payload = |text: &str| {
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: text.into(),
                is_final: false,
            })
        };
        let mut model = TimelineModel::default();
        model.replace_authoritative(session.clone(), [item(&session, 1, payload("one"))]);
        let revision = model.revision;
        let three = item(&session, 3, payload("three"));
        let two = item(&session, 2, payload("two"));

        assert_eq!(
            model.apply_live_batch([
                TimelineLiveEvent {
                    session_id: session.clone(),
                    sequence: three.sequence,
                    item: three.clone(),
                },
                TimelineLiveEvent {
                    session_id: session.clone(),
                    sequence: two.sequence,
                    item: two,
                },
            ]),
            2
        );
        assert_eq!(model.revision, revision.wrapping_add(1));
        assert_eq!(
            model
                .items
                .iter()
                .map(|item| item.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(model.needs_authoritative_refetch);

        let revision = model.revision;
        assert_eq!(
            model.apply_live_batch([TimelineLiveEvent {
                session_id: session.clone(),
                sequence: three.sequence,
                item: three,
            }]),
            0
        );
        assert_eq!(model.revision, revision);
    }

    #[test]
    fn identical_authoritative_replace_keeps_revision_and_clears_refetch() {
        let session = VibexSessionId::parse("session_idempotent").unwrap();
        let item = item(
            &session,
            1,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "done".into(),
                is_final: true,
            }),
        );
        let mut model = TimelineModel::default();
        model.replace_authoritative(session.clone(), [item.clone()]);
        let revision = model.revision;
        model.mark_lagged();

        model.replace_authoritative(session, [item]);

        assert_eq!(model.revision, revision);
        assert!(!model.needs_authoritative_refetch);
    }

    #[test]
    fn continuation_after_error_starts_a_new_turn_without_user_boundary() {
        let session = VibexSessionId::parse("session_current").unwrap();
        let mut model = TimelineModel::default();
        model.replace_authoritative(
            session.clone(),
            [
                item(
                    &session,
                    1,
                    TimelinePayload::UserMessage(UserMessagePayload {
                        text: "try it".into(),
                        attachments: Vec::new(),
                        ..Default::default()
                    }),
                ),
                item(
                    &session,
                    2,
                    TimelinePayload::Error(vibex_core::TimelineErrorPayload {
                        code: "turn_failed".into(),
                        message: "failed".into(),
                        recoverable: true,
                    }),
                ),
                item(
                    &session,
                    3,
                    TimelinePayload::AgentMessage(AgentMessagePayload {
                        text: "continued".into(),
                        is_final: true,
                    }),
                ),
            ],
        );
        let turns = model.turns();
        assert_eq!(turns.len(), 2);
        assert!(turns[0].failed);
        assert!(turns[1].user_item.is_none());
        assert_eq!(turns[1].conclusion_item_id.as_deref(), Some("timeline_3"));
    }

    #[test]
    fn provider_correlated_error_remains_process_state_inside_the_turn() {
        let session = VibexSessionId::parse("session_current").unwrap();
        let mut provider_error = item(
            &session,
            2,
            TimelinePayload::Error(vibex_core::TimelineErrorPayload {
                code: "provider_progress_error".into(),
                message: "retrying internally".into(),
                recoverable: true,
            }),
        );
        provider_error.provider_correlation_id = Some("provider-error".into());
        let turns = timeline_turns(&[
            item(
                &session,
                1,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "try it".into(),
                    attachments: Vec::new(),
                    ..Default::default()
                }),
            ),
            provider_error,
            item(
                &session,
                3,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "completed".into(),
                    is_final: true,
                }),
            ),
        ]);

        assert_eq!(turns.len(), 1);
        assert!(!turns[0].failed);
        assert_eq!(turns[0].conclusion_item_id.as_deref(), Some("timeline_3"));
    }

    #[test]
    fn permission_resolution_removes_pending_id_from_turn() {
        let session = VibexSessionId::parse("session_current").unwrap();
        let request_id = vibex_core::RequestId::parse("request_1").unwrap();
        let mut model = TimelineModel::default();
        model.replace_authoritative(
            session.clone(),
            [
                item(
                    &session,
                    1,
                    TimelinePayload::UserMessage(UserMessagePayload {
                        text: "approve".into(),
                        attachments: Vec::new(),
                        ..Default::default()
                    }),
                ),
                item(
                    &session,
                    2,
                    TimelinePayload::PermissionRequest(vibex_core::PermissionRequest {
                        id: request_id.clone(),
                        session_id: session.clone(),
                        project_id: None,
                        workspace_id: None,
                        provider_request_id: None,
                        risk_category: vibex_core::PermissionRiskCategory::Command,
                        title: "Run command?".into(),
                        details: Vec::new(),
                        allowed_responses: vec![
                            vibex_core::PermissionResponseKind::Approve,
                            vibex_core::PermissionResponseKind::Deny,
                        ],
                        response_options: Vec::new(),
                        status: PermissionRequestStatus::Pending,
                        requested_at_ms: 2,
                        expires_at_ms: None,
                    }),
                ),
                item(
                    &session,
                    3,
                    TimelinePayload::PermissionResolution(vibex_core::PermissionResolution {
                        request_id,
                        session_id: session.clone(),
                        response: vibex_core::PermissionResponseKind::Approve,
                        responder_device_id: None,
                        provider_resolution_id: None,
                        note: None,
                        resolved_at_ms: 3,
                    }),
                ),
            ],
        );
        assert!(model.turns()[0].pending_permission_ids.is_empty());
    }

    #[test]
    fn a_gap_ends_the_contiguous_run() {
        let session = VibexSessionId::parse("session_contiguous").unwrap();
        let mut model = TimelineModel::default();
        model.replace_authoritative(
            session.clone(),
            [
                item(
                    &session,
                    1,
                    TimelinePayload::AgentMessage(AgentMessagePayload {
                        text: "one".into(),
                        is_final: true,
                    }),
                ),
                item(
                    &session,
                    2,
                    TimelinePayload::AgentMessage(AgentMessagePayload {
                        text: "two".into(),
                        is_final: true,
                    }),
                ),
                item(
                    &session,
                    5,
                    TimelinePayload::AgentMessage(AgentMessagePayload {
                        text: "five".into(),
                        is_final: true,
                    }),
                ),
            ],
        );

        assert_eq!(model.contiguous_end_sequence(), Some(2));
        assert!(!model.is_contiguous());
    }

    #[test]
    fn sequence_gap_marks_authoritative_refetch_without_accepting_other_sessions() {
        let session = VibexSessionId::parse("session_current").unwrap();
        let other = VibexSessionId::parse("session_other").unwrap();
        let mut model = TimelineModel::default();
        model.replace_authoritative(
            session.clone(),
            [item(
                &session,
                1,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "one".into(),
                    is_final: true,
                }),
            )],
        );
        let gap = item(
            &session,
            3,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "three".into(),
                is_final: true,
            }),
        );
        assert!(model.apply_live(TimelineLiveEvent {
            session_id: session.clone(),
            sequence: 3,
            item: gap,
        }));
        assert!(model.needs_authoritative_refetch);

        let stale = item(
            &other,
            4,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "stale".into(),
                is_final: true,
            }),
        );
        assert!(!model.apply_live(TimelineLiveEvent {
            session_id: other,
            sequence: 4,
            item: stale,
        }));
        assert_eq!(model.items.len(), 2);
    }

    /// An abandoned card from a finished turn must not outlive it.
    ///
    /// This is the desktop half of the stuck `?` row: the runtime cancels an
    /// interrupted request without appending a resolution, so the request item
    /// stays `Pending` forever. Counting only the live turn's requests is what
    /// lets a later prompt — or an authoritative reload — clear it.
    #[test]
    fn live_turn_pending_ids_ignore_an_earlier_turns_abandoned_request() {
        let session = VibexSessionId::parse("session_current").unwrap();
        let abandoned = vibex_core::RequestId::parse("request_abandoned").unwrap();
        let current = vibex_core::RequestId::parse("request_current").unwrap();
        let permission_request = |id: &vibex_core::RequestId| {
            TimelinePayload::PermissionRequest(vibex_core::PermissionRequest {
                id: id.clone(),
                session_id: session.clone(),
                project_id: None,
                workspace_id: None,
                provider_request_id: None,
                risk_category: vibex_core::PermissionRiskCategory::Command,
                title: "Run command?".into(),
                details: Vec::new(),
                allowed_responses: vec![vibex_core::PermissionResponseKind::Approve],
                response_options: Vec::new(),
                status: PermissionRequestStatus::Pending,
                requested_at_ms: 1,
                expires_at_ms: None,
            })
        };
        let prompt = |text: &str, delivery| {
            TimelinePayload::UserMessage(UserMessagePayload {
                text: text.into(),
                attachments: Vec::new(),
                delivery,
                ..Default::default()
            })
        };
        let mut items = vec![
            item(&session, 1, prompt("first", UserMessageDelivery::Prompt)),
            item(&session, 2, permission_request(&abandoned)),
            item(&session, 3, prompt("second", UserMessageDelivery::Prompt)),
            item(&session, 4, permission_request(&current)),
        ];
        assert_eq!(
            live_turn_pending_permission_ids(&items),
            BTreeSet::from([current.to_string()]),
            "a new prompt supersedes the request the previous turn left behind"
        );

        // A steer joins the running turn, so the card asked before the steer is
        // still the live question.
        let steered = vec![
            item(&session, 1, prompt("first", UserMessageDelivery::Prompt)),
            item(&session, 2, permission_request(&abandoned)),
            item(&session, 3, prompt("aside", UserMessageDelivery::Steer)),
        ];
        assert_eq!(
            live_turn_pending_permission_ids(&steered),
            BTreeSet::from([abandoned.to_string()])
        );

        // A resolution inside the live turn still clears its own request.
        items.push(item(
            &session,
            5,
            TimelinePayload::PermissionResolution(vibex_core::PermissionResolution {
                request_id: current.clone(),
                session_id: session.clone(),
                response: vibex_core::PermissionResponseKind::Deny,
                responder_device_id: None,
                provider_resolution_id: None,
                note: None,
                resolved_at_ms: 5,
            }),
        ));
        assert!(live_turn_pending_permission_ids(&items).is_empty());

        // A window with no turn boundary keeps the requests it opened with.
        assert_eq!(
            live_turn_pending_permission_ids(&[item(&session, 2, permission_request(&abandoned))]),
            BTreeSet::from([abandoned.to_string()])
        );
    }
}

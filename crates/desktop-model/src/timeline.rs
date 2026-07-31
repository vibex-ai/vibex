use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentSessionState, PermissionRequestStatus, TimelineItem, TimelineItemKind, TimelineLiveEvent,
    TimelinePayload, VibexSessionId,
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
        self.session_id = Some(session_id.clone());
        self.items = normalize_items(session_id, items);
        self.authoritative_end_sequence = self.items.last().map(|item| item.sequence);
        self.needs_authoritative_refetch = false;
        self.revision = self.revision.wrapping_add(1);
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
                _ => {}
            }
        }
        pending
    }
}

pub fn timeline_turns(items: &[TimelineItem]) -> Vec<TimelineTurn> {
    let mut turns = Vec::new();
    let mut current = empty_turn();
    for item in items {
        if item.kind == TimelineItemKind::UserMessage {
            if current.user_item.is_some() || !current.response_items.is_empty() {
                finish_turn(&mut current);
                turns.push(current);
                current = empty_turn();
            }
            current.user_item = Some(item.clone());
        } else {
            if turn_has_error(&current)
                && item.kind != TimelineItemKind::Error
                && matches!(
                    item.source,
                    vibex_core::TimelineSource::Agent | vibex_core::TimelineSource::Provider
                )
            {
                finish_turn(&mut current);
                turns.push(current);
                current = empty_turn();
            }
            current.response_items.push(item.clone());
        }
    }
    if current.user_item.is_some() || !current.response_items.is_empty() {
        finish_turn(&mut current);
        turns.push(current);
    }
    turns
}

fn empty_turn() -> TimelineTurn {
    TimelineTurn {
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

fn finish_turn(turn: &mut TimelineTurn) {
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
            _ => {}
        }
    }
    turn.pending_permission_ids = pending_permission_ids.into_iter().collect();
    turn.failed = turn.response_items.iter().any(is_turn_boundary_error);
    turn.conclusion_item_id = turn
        .response_items
        .iter()
        .rev()
        .find(|item| is_turn_boundary_error(item) || is_final_agent_message(item))
        .map(|item| item.id.to_string());
}

fn turn_has_error(turn: &TimelineTurn) -> bool {
    turn.response_items.iter().any(is_turn_boundary_error)
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
}

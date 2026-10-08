use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use vibex_core::{
    AgentSessionState, TimelineItem, TimelineLiveEvent, VibexError, VibexResult, VibexSessionId,
};
use vibex_desktop_model::{
    ReasoningDisplayMode, TimelineConversationTurn, TimelineModel, TimelineRow,
};

use super::{ConversationTurnsSummary, VibexWorkbench, reset_pending_user_requests};

/// A history snapshot prepared away from the UI thread. The UI only installs
/// the normalized model and retained turn handles once the request is current.
pub(super) struct PreparedTimeline {
    timeline: TimelineModel,
    turns: Vec<TimelineConversationTurn>,
    state: Option<AgentSessionState>,
    pending: bool,
    reasoning: ReasoningDisplayMode,
}

/// Only the request owns this buffer. Its weak view reference expires on
/// cancellation, so inactive sessions never accumulate an abandoned event log.
pub(super) struct TimelineLoadUpdates {
    revision: u64,
    invalidated: bool,
    items: BTreeMap<i64, TimelineItem>,
}

impl TimelineLoadUpdates {
    pub(super) fn record(&mut self, events: &[TimelineLiveEvent], revision: u64) {
        self.invalidated |= self.revision != revision;
        for event in events {
            self.items.insert(event.sequence, event.item.clone());
        }
    }

    pub(super) fn advance(&mut self, revision: u64) {
        self.revision = revision;
    }
}

impl PreparedTimeline {
    fn replay_live_updates(&mut self, updates: &mut TimelineLoadUpdates) {
        let Some(session_id) = self.timeline.session_id.clone() else {
            return;
        };
        let replayed =
            self.timeline
                .apply_live_batch(
                    std::mem::take(&mut updates.items)
                        .into_values()
                        .map(|item| TimelineLiveEvent {
                            session_id: session_id.clone(),
                            sequence: item.sequence,
                            item,
                        }),
                );
        if replayed > 0 {
            // Only a read overlapping a live batch needs a catch-up projection.
            // The common history path arrives completely prepared.
            self.turns = self.timeline.conversation_turns_with_reasoning_mode(
                self.state,
                self.pending,
                self.reasoning,
            );
        }
    }

    pub(super) async fn prepare(
        session_id: VibexSessionId,
        items: Vec<TimelineItem>,
        state: Option<AgentSessionState>,
        pending: bool,
        reasoning: ReasoningDisplayMode,
    ) -> VibexResult<Self> {
        tokio::task::spawn_blocking(move || {
            let mut timeline = TimelineModel::default();
            timeline.replace_authoritative(session_id, items);
            let turns = timeline.conversation_turns_with_reasoning_mode(state, pending, reasoning);
            Self {
                timeline,
                turns,
                state,
                pending,
                reasoning,
            }
        })
        .await
        .map_err(|_| {
            VibexError::storage(
                "timeline_preparation_failed",
                "Could not prepare conversation history",
            )
        })
    }
}

impl VibexWorkbench {
    pub(super) fn begin_timeline_load(&mut self) -> Rc<RefCell<TimelineLoadUpdates>> {
        let updates = Rc::new(RefCell::new(TimelineLoadUpdates {
            revision: self.timeline.revision,
            invalidated: false,
            items: BTreeMap::new(),
        }));
        self.timeline_load_updates = Rc::downgrade(&updates);
        updates
    }

    pub(super) fn install_prepared_timeline(
        &mut self,
        mut prepared: PreparedTimeline,
        updates: &mut TimelineLoadUpdates,
    ) -> bool {
        // Edits/replacements outside the live stream invalidate the read. Live
        // events, including same-sequence revisions, are replayed over it.
        if updates.invalidated || self.timeline.revision != updates.revision {
            return false;
        }
        let Some(session_id) = prepared.timeline.session_id.clone() else {
            return false;
        };
        prepared.replay_live_updates(updates);
        self.capture_timeline_scroll_anchor();
        reset_pending_user_requests(
            &mut self.pending_user_request_ids,
            &session_id,
            &prepared.timeline.items,
        );
        let changed = self.timeline.session_id != prepared.timeline.session_id
            || self.timeline.items != prepared.timeline.items;
        if changed {
            let (turns, changed_turns) =
                reconcile_prepared_turns(&self.conversation_turns_cache, prepared.turns);
            let previous = self.conversation_turns_cache.clone();
            self.invalidate_changed_timeline_content(&previous, &turns);
            for id in changed_turns {
                self.remeasure_timeline_turn(&id);
                self.timeline_estimated_turn_heights.remove(&id);
            }
            prepared.timeline.revision = self.timeline.revision.wrapping_add(1);
            self.timeline = prepared.timeline;
            self.timeline_item_index.get_mut().invalidate();
            self.invalidate_agent_generation_output_estimate();
            self.invalidate_agent_generation_compaction_count();
            self.conversation_turns_summary = ConversationTurnsSummary::from_turns(&turns);
            self.conversation_turns_cache = Rc::new(turns);
            let key = self.current_conversation_turns_cache_key();
            self.conversation_turns_cache_key = (key.session_state == prepared.state
                && key.agent_turn_pending == prepared.pending
                && key.reasoning_display_mode == prepared.reasoning
                && key.pending_edit.is_none()
                && key.optimistic_message.is_none())
            .then_some(key);
            self.timeline_turn_layout_signature_cache.clear();
            self.sync_conversation_turns_render_cache();
        } else {
            self.timeline.needs_authoritative_refetch =
                prepared.timeline.needs_authoritative_refetch;
        }
        self.reconcile_optimistic_user_message();
        true
    }

    /// A same-sequence tool update can replace its output without changing a
    /// projection cache key. Evict only changed row projections; retained text
    /// entities apply the new source incrementally on their next render. Keep
    /// measured geometry until that render instead of replacing it with estimates.
    pub(super) fn invalidate_changed_timeline_content(
        &mut self,
        previous: &[Rc<TimelineConversationTurn>],
        incoming: &[Rc<TimelineConversationTurn>],
    ) {
        let incoming_rows = timeline_rows_by_id(incoming);
        for (id, row) in timeline_rows_by_id(previous) {
            let new = incoming_rows.get(id);
            if new.is_some_and(|new| *new == row) {
                continue;
            }
            self.timeline_markdown_sources.remove(id);
            self.timeline_reasoning_summaries.remove(id);
            self.timeline_tool_card_projections.remove(id);
            self.timeline_file_diff_previews.remove(id);
            if new.is_none() {
                self.timeline_process_unit_heights.remove(id);
            } else if let Some(height) = self.timeline_process_unit_heights.get_mut(id) {
                height.layout_invalidated = true;
            }
        }
        let incoming: BTreeMap<_, _> = incoming
            .iter()
            .map(|turn| (turn.id.as_str(), turn))
            .collect();
        for turn in previous {
            if incoming
                .get(turn.id.as_str())
                .is_none_or(|new| new.as_ref() != turn.as_ref())
            {
                self.timeline_turn_file_changes.remove(&turn.id);
                self.timeline_markdown_sources
                    .remove(&format!("reasoning-live:{}", turn.id));
                if incoming.contains_key(turn.id.as_str()) {
                    self.remeasure_timeline_turn(&turn.id);
                } else {
                    self.invalidate_timeline_turn_measurement(&turn.id);
                }
                self.timeline_estimated_turn_heights.remove(&turn.id);
            }
        }
    }
}

fn timeline_rows_by_id(turns: &[Rc<TimelineConversationTurn>]) -> BTreeMap<&str, &TimelineRow> {
    turns
        .iter()
        .flat_map(|turn| {
            turn.user_row
                .iter()
                .chain(&turn.process_rows)
                .chain(turn.conclusion_row.iter())
        })
        .map(|row| (row.id.as_str(), row))
        .collect()
}

fn reconcile_prepared_turns(
    previous: &[Rc<TimelineConversationTurn>],
    incoming: Vec<TimelineConversationTurn>,
) -> (Vec<Rc<TimelineConversationTurn>>, Vec<String>) {
    let previous: BTreeMap<_, _> = previous
        .iter()
        .map(|turn| (turn.id.as_str(), turn))
        .collect();
    let mut changed = Vec::new();
    let turns = incoming
        .into_iter()
        .map(|turn| {
            if let Some(old) = previous
                .get(turn.id.as_str())
                .filter(|old| old.as_ref() == &turn)
            {
                Rc::clone(old)
            } else {
                changed.push(turn.id.clone());
                Rc::new(turn)
            }
        })
        .collect();
    (turns, changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentMessagePayload, TimelineItemId, TimelinePayload, TimelineRedactionState,
        TimelineSource, UserMessagePayload,
    };

    fn item(session: &VibexSessionId, sequence: i64, user: bool, text: &str) -> TimelineItem {
        let payload = if user {
            TimelinePayload::UserMessage(UserMessagePayload {
                text: text.into(),
                ..Default::default()
            })
        } else {
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: text.into(),
                is_final: true,
            })
        };
        TimelineItem {
            id: TimelineItemId::new(),
            session_id: session.clone(),
            sequence,
            timestamp_ms: sequence,
            source: if user {
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

    #[tokio::test]
    async fn history_reuses_unchanged_turn_handles_after_a_later_answer_changes() {
        let session = VibexSessionId::new();
        let mut items = vec![
            item(&session, 1, true, "first"),
            item(&session, 2, false, "answer one"),
            item(&session, 3, true, "second"),
            item(&session, 4, false, "answer two"),
        ];
        let initial = PreparedTimeline::prepare(
            session.clone(),
            items.clone(),
            None,
            false,
            ReasoningDisplayMode::LatestAtBottom,
        )
        .await
        .unwrap();
        let previous: Vec<_> = initial.turns.into_iter().map(Rc::new).collect();
        assert_eq!(previous.len(), 2);
        items[3].payload = TimelinePayload::AgentMessage(AgentMessagePayload {
            text: "corrected answer".into(),
            is_final: true,
        });
        let incoming = PreparedTimeline::prepare(
            session,
            items,
            None,
            false,
            ReasoningDisplayMode::LatestAtBottom,
        )
        .await
        .unwrap();
        let (turns, changed) = reconcile_prepared_turns(&previous, incoming.turns);
        assert!(Rc::ptr_eq(&previous[0], &turns[0]));
        assert!(!Rc::ptr_eq(&previous[1], &turns[1]));
        assert_eq!(changed, vec![turns[1].id.clone()]);
    }

    #[tokio::test]
    async fn history_replays_appends_and_same_sequence_live_revisions() {
        let session = VibexSessionId::new();
        let first = item(&session, 1, true, "question");
        let mut answer = item(&session, 2, false, "old answer");
        let mut prepared = PreparedTimeline::prepare(
            session.clone(),
            vec![first, answer.clone()],
            None,
            false,
            ReasoningDisplayMode::LatestAtBottom,
        )
        .await
        .unwrap();
        answer.payload = TimelinePayload::AgentMessage(AgentMessagePayload {
            text: "live answer".into(),
            is_final: true,
        });
        let appended = item(&session, 3, true, "next question");
        let mut updates = TimelineLoadUpdates {
            revision: 1,
            invalidated: false,
            items: BTreeMap::new(),
        };
        updates.record(
            &[answer.clone(), appended.clone()]
                .into_iter()
                .map(|item| TimelineLiveEvent {
                    session_id: session.clone(),
                    sequence: item.sequence,
                    item,
                })
                .collect::<Vec<_>>(),
            1,
        );
        updates.advance(2);
        prepared.replay_live_updates(&mut updates);
        assert_eq!(prepared.timeline.items.len(), 3);
        assert_eq!(prepared.timeline.items[1], answer);
        assert_eq!(prepared.timeline.items[2], appended);
        assert!(!prepared.timeline.needs_authoritative_refetch);
        assert!(updates.items.is_empty());
    }

    #[test]
    fn non_live_replacements_invalidate_a_history_read_and_cancellation_releases_its_buffer() {
        let updates = Rc::new(RefCell::new(TimelineLoadUpdates {
            revision: 1,
            invalidated: false,
            items: BTreeMap::new(),
        }));
        let weak = Rc::downgrade(&updates);
        updates.borrow_mut().record(&[], 2);
        updates.borrow_mut().advance(3);
        assert!(updates.borrow().invalidated);
        drop(updates);
        assert!(weak.upgrade().is_none());
    }
}

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use vibex_core::{RemoteEventV2, RemoteResyncRequired, RemoteStreamCursor};

/// A bounded, server-authoritative cursor for one mutable domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainCursorState {
    pub domain: String,
    pub generation: u64,
    pub cursor: u64,
    pub snapshot_version: Option<String>,
}

impl DomainCursorState {
    pub fn new(domain: impl Into<String>) -> Self {
        Self {
            domain: domain.into(),
            generation: 0,
            cursor: 0,
            snapshot_version: None,
        }
    }

    pub fn as_stream_cursor(&self) -> RemoteStreamCursor {
        RemoteStreamCursor {
            domain: self.domain.clone(),
            generation: self.generation,
            cursor: self.cursor,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncDecision {
    /// The event is the next authoritative event and may be applied.
    Apply,
    /// The event was already applied (or is an exact replay).
    IgnoreDuplicate,
    /// The event belongs to an older connection generation.
    IgnoreStaleGeneration,
    /// A bounded catch-up request should start at `after_cursor`.
    CatchUp {
        domain: String,
        generation: u64,
        after_cursor: u64,
    },
    /// The retention window or generation is no longer usable.  The caller
    /// must refetch the authoritative projection before resubscribing.
    Resync {
        domain: String,
        generation: u64,
        reason: String,
        authoritative_operation: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncApplyError {
    UnknownDomain(String),
    SnapshotGenerationRewound {
        domain: String,
        current: u64,
        received: u64,
    },
    SnapshotCursorRewound {
        domain: String,
        generation: u64,
        current: u64,
        received: u64,
    },
}

/// Domain synchronisation shared by Direct and future Relay transports.
///
/// The engine never treats a live event as the source of truth by itself.  It
/// advances a cursor only for contiguous events and explicitly asks its owner
/// to catch up or refetch on a gap.
#[derive(Debug, Clone)]
pub struct DomainSyncEngine {
    domains: BTreeMap<String, DomainCursorState>,
    ephemeral_domains: BTreeSet<String>,
    pending_events: VecDeque<RemoteEventV2>,
    max_pending_events: usize,
    dropped_events: u64,
    paused: bool,
}

impl Default for DomainSyncEngine {
    fn default() -> Self {
        Self::new(256)
    }
}

impl DomainSyncEngine {
    pub fn new(max_pending_events: usize) -> Self {
        Self {
            domains: BTreeMap::new(),
            ephemeral_domains: BTreeSet::new(),
            pending_events: VecDeque::new(),
            max_pending_events: max_pending_events.max(1),
            dropped_events: 0,
            paused: false,
        }
    }

    pub fn register_domain(&mut self, domain: impl Into<String>) {
        let domain = domain.into();
        self.domains
            .entry(domain.clone())
            .or_insert_with(|| DomainCursorState::new(domain));
    }

    pub fn register_domains<I, S>(&mut self, domains: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for domain in domains {
            self.register_domain(domain);
        }
    }

    /// Ephemeral domains use contiguous sequencing only for the current wire
    /// connection. They have no authoritative catch-up source, so their
    /// cursors must never survive reconnect or route handoff.
    pub fn register_ephemeral_domain(&mut self, domain: impl Into<String>) {
        let domain = domain.into();
        self.register_domain(domain.clone());
        self.ephemeral_domains.insert(domain);
    }

    /// Seed a fresh transport with cursors committed by the shared backend.
    /// Route handoff uses this before hello/subscribe so Direct and Relay ask
    /// the PC for authoritative catch-up from the same applied position.
    pub fn seed_cursors(&mut self, cursors: &[RemoteStreamCursor]) {
        for cursor in cursors {
            if self.ephemeral_domains.contains(&cursor.domain) {
                continue;
            }
            let state = self
                .domains
                .entry(cursor.domain.clone())
                .or_insert_with(|| DomainCursorState::new(cursor.domain.clone()));
            if cursor.generation > state.generation
                || (cursor.generation == state.generation && cursor.cursor > state.cursor)
            {
                state.generation = cursor.generation;
                state.cursor = cursor.cursor;
                state.snapshot_version = None;
            }
        }
    }

    pub fn domain(&self, domain: &str) -> Option<&DomainCursorState> {
        self.domains.get(domain)
    }

    pub fn cursors(&self) -> Vec<RemoteStreamCursor> {
        self.domains
            .values()
            .filter(|state| !self.ephemeral_domains.contains(&state.domain))
            .map(DomainCursorState::as_stream_cursor)
            .collect()
    }

    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn resume_after_refetch(&mut self) {
        self.paused = false;
        self.pending_events.clear();
    }

    /// A server/session epoch change invalidates every domain cursor.  The
    /// next live event must establish a fresh generation or trigger an
    /// authoritative refetch; stale events from the prior session are never
    /// applied to the new projection.
    pub fn reset_for_session_epoch(&mut self) {
        for cursor in self.domains.values_mut() {
            cursor.generation = 0;
            cursor.cursor = 0;
            cursor.snapshot_version = None;
        }
        self.paused = false;
        self.pending_events.clear();
    }

    /// A disconnected consumer may not have applied every event that reached
    /// the transport queue.  Rewind local cursors before the next handshake
    /// so the server performs authoritative catch-up instead of treating a
    /// received-but-unconsumed event as committed.
    pub fn reset_for_reconnect(&mut self) {
        for domain in &self.ephemeral_domains {
            if let Some(cursor) = self.domains.get_mut(domain) {
                cursor.generation = 0;
                cursor.cursor = 0;
                cursor.snapshot_version = None;
            }
        }
        self.paused = false;
        self.pending_events.clear();
    }

    pub fn pause_for_resync(&mut self) {
        self.paused = true;
    }

    /// Observe a v2 live event and return the action required by the owner.
    pub fn observe(&mut self, event: RemoteEventV2) -> SyncDecision {
        let domain = event.channel.clone();
        let Some((cursor_domain, cursor_generation, cursor_cursor)) = self
            .domains
            .get(&domain)
            .map(|cursor| (cursor.domain.clone(), cursor.generation, cursor.cursor))
        else {
            self.register_domain(domain.clone());
            return SyncDecision::Resync {
                domain,
                generation: event.generation,
                reason: "event arrived for an unregistered domain".to_string(),
                authoritative_operation: "info".to_string(),
            };
        };

        if self.paused {
            self.enqueue(event);
            return SyncDecision::Resync {
                domain: domain.clone(),
                generation: cursor_generation,
                reason: "domain sync is paused pending authoritative refetch".to_string(),
                authoritative_operation: cursor_domain.clone(),
            };
        }

        if cursor_generation == 0 && cursor_cursor == 0 {
            if event.sequence != 1 {
                self.pause_for_resync();
                let generation = event.generation;
                self.enqueue(event);
                return SyncDecision::CatchUp {
                    domain: cursor_domain,
                    generation,
                    after_cursor: 0,
                };
            }
            let cursor = self
                .domains
                .get_mut(&domain)
                .expect("domain was registered");
            cursor.generation = event.generation;
            cursor.cursor = event.sequence;
            self.enqueue(event);
            return SyncDecision::Apply;
        }

        if event.generation < cursor_generation {
            return SyncDecision::IgnoreStaleGeneration;
        }

        if event.generation > cursor_generation {
            self.pause_for_resync();
            self.enqueue(event.clone());
            return SyncDecision::Resync {
                domain: domain.clone(),
                generation: event.generation,
                reason: "event generation changed".to_string(),
                authoritative_operation: cursor_domain.clone(),
            };
        }

        if event.sequence <= cursor_cursor {
            return SyncDecision::IgnoreDuplicate;
        }

        let expected = cursor_cursor.saturating_add(1);
        if event.sequence != expected {
            self.pause_for_resync();
            self.enqueue(event);
            return SyncDecision::CatchUp {
                domain: cursor_domain,
                generation: cursor_generation,
                after_cursor: cursor_cursor,
            };
        }

        let cursor = self
            .domains
            .get_mut(&domain)
            .expect("domain was registered");
        cursor.cursor = event.sequence;
        self.enqueue(event);
        SyncDecision::Apply
    }

    /// Projection invalidations carry no business state, so multiple missed
    /// notifications coalesce into one authoritative refetch. Advancing the
    /// cursor across a gap is safe for these domains and must not pause Agent
    /// timeline synchronization.
    pub fn observe_invalidation(&mut self, event: RemoteEventV2) -> SyncDecision {
        let domain = event.channel.clone();
        let cursor = self
            .domains
            .entry(domain.clone())
            .or_insert_with(|| DomainCursorState::new(domain));
        if event.generation < cursor.generation {
            return SyncDecision::IgnoreStaleGeneration;
        }
        if event.generation == cursor.generation && event.sequence <= cursor.cursor {
            return SyncDecision::IgnoreDuplicate;
        }
        cursor.generation = event.generation;
        cursor.cursor = event.sequence;
        cursor.snapshot_version = None;
        SyncDecision::Apply
    }

    /// Apply a bounded replay page.  Replay pages may start after the current
    /// cursor and are checked for contiguous sequence numbers just like live
    /// events.  A retention miss asks the caller to replace the projection.
    pub fn apply_replay_page(
        &mut self,
        domain: &str,
        generation: u64,
        events: impl IntoIterator<Item = RemoteEventV2>,
        compacted: bool,
    ) -> Result<usize, SyncApplyError> {
        let (current_generation, current_cursor) = self
            .domains
            .get(domain)
            .map(|cursor| (cursor.generation, cursor.cursor))
            .ok_or_else(|| SyncApplyError::UnknownDomain(domain.to_string()))?;
        if generation < current_generation {
            return Ok(0);
        }
        if generation > current_generation && current_cursor > 0 {
            return Err(SyncApplyError::SnapshotGenerationRewound {
                domain: domain.to_string(),
                current: current_generation,
                received: generation,
            });
        }
        if generation > current_generation {
            self.domains
                .get_mut(domain)
                .expect("domain was registered")
                .generation = generation;
        }
        let mut applied = 0;
        for event in events {
            if event.channel != domain || event.generation != generation {
                continue;
            }
            let current_cursor = self
                .domains
                .get(domain)
                .map(|state| state.cursor)
                .unwrap_or(0);
            if event.sequence <= current_cursor {
                continue;
            }
            if event.sequence != current_cursor.saturating_add(1) {
                self.pause_for_resync();
                return Ok(applied);
            }
            let cursor = self.domains.get_mut(domain).expect("domain was registered");
            cursor.generation = generation;
            cursor.cursor = event.sequence;
            applied += 1;
        }
        if compacted {
            self.pause_for_resync();
        } else {
            self.resume_after_refetch();
        }
        Ok(applied)
    }

    pub fn apply_snapshot(
        &mut self,
        domain: &str,
        generation: u64,
        cursor: u64,
        snapshot_version: Option<String>,
    ) -> Result<(), SyncApplyError> {
        let state = self
            .domains
            .get_mut(domain)
            .ok_or_else(|| SyncApplyError::UnknownDomain(domain.to_string()))?;
        if generation < state.generation {
            return Ok(());
        }
        if generation == state.generation && cursor < state.cursor {
            return Err(SyncApplyError::SnapshotCursorRewound {
                domain: domain.to_string(),
                generation,
                current: state.cursor,
                received: cursor,
            });
        }
        state.generation = generation;
        state.cursor = cursor;
        state.snapshot_version = snapshot_version;
        self.resume_after_refetch();
        Ok(())
    }

    pub fn take_pending(&mut self) -> Vec<RemoteEventV2> {
        self.pending_events.drain(..).collect()
    }

    fn enqueue(&mut self, event: RemoteEventV2) {
        if self.pending_events.iter().any(|pending| {
            pending.channel == event.channel
                && pending.generation == event.generation
                && pending.sequence == event.sequence
        }) {
            return;
        }
        if self.pending_events.len() >= self.max_pending_events {
            self.pending_events.pop_front();
            self.dropped_events = self.dropped_events.saturating_add(1);
        }
        self.pending_events.push_back(event);
    }
}

impl From<RemoteResyncRequired> for SyncDecision {
    fn from(value: RemoteResyncRequired) -> Self {
        SyncDecision::Resync {
            domain: value.domain,
            generation: value.generation,
            reason: value.reason,
            authoritative_operation: value.authoritative_operation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{EventId, unix_timestamp_ms};

    fn event(domain: &str, generation: u64, sequence: u64) -> RemoteEventV2 {
        RemoteEventV2 {
            event_id: EventId::new(),
            channel: domain.to_string(),
            generation,
            sequence,
            correlation_id: None,
            payload: None,
            emitted_at_ms: unix_timestamp_ms(),
        }
    }

    #[test]
    fn contiguous_events_apply_and_duplicates_are_ignored() {
        let mut engine = DomainSyncEngine::new(8);
        engine.register_domain("agent_session");
        assert_eq!(
            engine.observe(event("agent_session", 1, 1)),
            SyncDecision::Apply
        );
        assert_eq!(
            engine.observe(event("agent_session", 1, 1)),
            SyncDecision::IgnoreDuplicate
        );
        assert_eq!(engine.domain("agent_session").unwrap().cursor, 1);
    }

    #[test]
    fn a_gap_pauses_and_requests_bounded_catch_up() {
        let mut engine = DomainSyncEngine::new(8);
        engine.register_domain("agent_session");
        engine.apply_snapshot("agent_session", 1, 1, None).unwrap();
        assert_eq!(
            engine.observe(event("agent_session", 4, 2)),
            SyncDecision::Resync {
                domain: "agent_session".to_string(),
                generation: 4,
                reason: "event generation changed".to_string(),
                authoritative_operation: "agent_session".to_string(),
            }
        );
        engine.apply_snapshot("agent_session", 4, 2, None).unwrap();
        assert_eq!(
            engine.observe(event("agent_session", 4, 4)),
            SyncDecision::CatchUp {
                domain: "agent_session".to_string(),
                generation: 4,
                after_cursor: 2,
            }
        );
        assert!(engine.is_paused());
    }

    #[test]
    fn projection_invalidation_gaps_coalesce_without_pausing_other_domains() {
        let mut engine = DomainSyncEngine::new(8);
        engine.register_domains(["file", "agent_session"]);

        assert_eq!(
            engine.observe_invalidation(event("file", 3, 9)),
            SyncDecision::Apply
        );
        assert!(!engine.is_paused());
        assert_eq!(engine.domain("file").unwrap().generation, 3);
        assert_eq!(engine.domain("file").unwrap().cursor, 9);
        assert_eq!(
            engine.observe_invalidation(event("file", 3, 8)),
            SyncDecision::IgnoreDuplicate
        );
        assert_eq!(
            engine.observe(event("agent_session", 3, 1)),
            SyncDecision::Apply
        );
    }

    #[test]
    fn pending_events_are_bounded_and_report_drops() {
        let mut engine = DomainSyncEngine::new(2);
        engine.register_domain("agent_session");
        engine.apply_snapshot("agent_session", 1, 0, None).unwrap();
        assert_eq!(
            engine.observe(event("agent_session", 1, 1)),
            SyncDecision::Apply
        );
        assert_eq!(
            engine.observe(event("agent_session", 1, 2)),
            SyncDecision::Apply
        );
        assert_eq!(
            engine.observe(event("agent_session", 1, 3)),
            SyncDecision::Apply
        );
        assert_eq!(engine.dropped_events(), 1);
        assert_eq!(engine.take_pending().len(), 2);
    }

    #[test]
    fn snapshot_cannot_rewind_a_cursor_within_the_same_generation() {
        let mut engine = DomainSyncEngine::new(4);
        engine.register_domain("agent_session");
        engine
            .apply_snapshot("agent_session", 3, 9, Some("snapshot-9".to_string()))
            .unwrap();
        assert!(matches!(
            engine.apply_snapshot("agent_session", 3, 8, None),
            Err(SyncApplyError::SnapshotCursorRewound {
                generation: 3,
                current: 9,
                received: 8,
                ..
            })
        ));
    }

    #[test]
    fn paused_queue_deduplicates_exact_event_replays() {
        let mut engine = DomainSyncEngine::new(4);
        engine.register_domain("agent_session");
        engine.apply_snapshot("agent_session", 1, 1, None).unwrap();
        let gap = event("agent_session", 1, 3);
        assert!(matches!(
            engine.observe(gap.clone()),
            SyncDecision::CatchUp { .. }
        ));
        assert!(matches!(engine.observe(gap), SyncDecision::Resync { .. }));
        assert_eq!(engine.take_pending().len(), 1);
    }

    #[test]
    fn session_epoch_reset_discards_all_domain_cursors() {
        let mut engine = DomainSyncEngine::new(4);
        engine.register_domain("agent_session");
        engine
            .apply_snapshot("agent_session", 9, 4, Some("v9".to_string()))
            .unwrap();
        engine.reset_for_session_epoch();
        let cursor = engine.domain("agent_session").unwrap();
        assert_eq!(cursor.generation, 0);
        assert_eq!(cursor.cursor, 0);
        assert!(cursor.snapshot_version.is_none());
    }

    #[test]
    fn route_handoff_preserves_committed_cursors_until_authoritative_resync() {
        let mut engine = DomainSyncEngine::new(8);
        engine.register_domain("agent_session");
        engine.seed_cursors(&[RemoteStreamCursor {
            domain: "agent_session".to_string(),
            generation: 7,
            cursor: 42,
        }]);

        engine.reset_for_reconnect();

        assert_eq!(engine.domain("agent_session").unwrap().generation, 7);
        assert_eq!(engine.domain("agent_session").unwrap().cursor, 42);
        assert_eq!(engine.cursors()[0].cursor, 42);
    }

    #[test]
    fn ephemeral_domain_cursor_is_scoped_to_one_connection() {
        let mut engine = DomainSyncEngine::new(8);
        engine.register_domain("agent_session");
        engine.register_ephemeral_domain("agent_notification");

        assert_eq!(
            engine.observe(event("agent_notification", 7, 1)),
            SyncDecision::Apply
        );
        assert_eq!(engine.domain("agent_notification").unwrap().cursor, 1);
        assert!(
            engine
                .cursors()
                .iter()
                .all(|cursor| cursor.domain != "agent_notification")
        );

        engine.seed_cursors(&[RemoteStreamCursor {
            domain: "agent_notification".to_string(),
            generation: 7,
            cursor: 42,
        }]);
        assert_eq!(engine.domain("agent_notification").unwrap().cursor, 1);

        engine.reset_for_reconnect();
        let cursor = engine.domain("agent_notification").unwrap();
        assert_eq!(cursor.generation, 0);
        assert_eq!(cursor.cursor, 0);
    }

    #[test]
    fn replay_pages_update_authority_without_replaying_events_to_wire_queue() {
        let mut engine = DomainSyncEngine::new(4);
        engine.register_domain("agent_session");
        let replay = [event("agent_session", 1, 1), event("agent_session", 1, 2)];
        assert_eq!(
            engine
                .apply_replay_page("agent_session", 1, replay, false)
                .unwrap(),
            2
        );
        assert_eq!(engine.domain("agent_session").unwrap().cursor, 2);
        assert!(engine.take_pending().is_empty());
    }
}

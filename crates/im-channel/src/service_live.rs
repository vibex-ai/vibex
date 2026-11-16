//! Live turn delivery: the progress line and the growing answer.
//!
//! A chat used to see only the final answer. The runtime already streams
//! `AgentMessageDelta` events, so a turn can be shown as it happens: one
//! message that a single worker owns and edits until the turn ends.
//!
//! The worker is the only writer. The event loop never blocks on an edit; it
//! records what the turn produced and makes sure a worker is running. That
//! split is what keeps a slow provider call from stalling every other chat on
//! the same timeline subscription, and it is why the edits can be coalesced:
//! the worker redraws from the latest state, so a burst of deltas costs one
//! edit instead of one edit each.

use vibex_core::{AgentMessageDeltaPayload, AgentMessagePhase, ImChannelKind, ImStreamMode};

use super::*;
use crate::error::ImResult;
use crate::types::ImActivity;

/// How long a live answer waits between edits.
///
/// A person does not read faster than this, and every edit is a provider call
/// that competes with approvals and final answers for the same quota.
const LIVE_EDIT_GAP_MS: i64 = 1_500;
/// How long a progress line waits between edits. Groups get the thriftier
/// setting because Telegram counts an edit against the group's message rate.
const PROGRESS_EDIT_GAP_MS: i64 = 4_000;
/// How often a progress line's clock is redrawn.
const PROGRESS_CLOCK_BUCKET_MS: i64 = 5_000;
/// How often the worker wakes to reconsider the message.
const LIVE_POLL_MS: u64 = 400;
/// How often the `typing` action is refreshed; Telegram clears it after ~5s.
const TYPING_TICK_MS: i64 = 4_000;
/// The longest a live message trails an answer that outgrew it.
const LIVE_TAIL_CHARS: usize = 2_800;
/// How long a finalizer waits for the worker to deliver before giving up.
const LIVE_FINISH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// What a chat actually asked to see, after `Auto` is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResolvedStream {
    Off,
    Typing,
    Progress,
    Live,
}

impl ResolvedStream {
    fn edit_gap_ms(self) -> i64 {
        match self {
            Self::Progress => PROGRESS_EDIT_GAP_MS,
            _ => LIVE_EDIT_GAP_MS,
        }
    }

    pub(super) fn edits_a_message(self) -> bool {
        matches!(self, Self::Progress | Self::Live)
    }
}

/// Resolves the channel's appetite for live updates against this chat.
///
/// `Auto` streams a direct message and keeps a group to a progress line. The
/// chat type is not always known: a timeline event names a binding, not an
/// inbound message, so a negative Telegram chat id or a thread key is read as a
/// group. Guessing "group" is the conservative direction, because it is the
/// answer that sends less.
pub(super) fn resolve_stream(
    mode: ImStreamMode,
    chat_type: Option<ImChatType>,
    group_hint: bool,
) -> ResolvedStream {
    match mode {
        ImStreamMode::Off => ResolvedStream::Off,
        ImStreamMode::Typing => ResolvedStream::Typing,
        ImStreamMode::Progress => ResolvedStream::Progress,
        ImStreamMode::Live => ResolvedStream::Live,
        ImStreamMode::Auto => {
            let group = match chat_type {
                Some(ImChatType::Direct) => false,
                Some(ImChatType::Group | ImChatType::Thread) => true,
                None => group_hint,
            };
            if group {
                ResolvedStream::Progress
            } else {
                ResolvedStream::Live
            }
        }
    }
}

/// Whether a chat looks like a group when the inbound message is gone.
///
/// A timeline event names a binding, not the message that created it. Telegram
/// ids carry the answer (groups are negative, forum topics have a thread key);
/// Lark's are opaque, so a binding without an inbound message takes the quieter
/// setting rather than editing a busy group's timeline.
fn group_hint(kind: ImChannelKind, external_chat_id: &str, thread_key: &str) -> bool {
    match kind {
        ImChannelKind::Telegram => external_chat_id.starts_with('-') || !thread_key.is_empty(),
        ImChannelKind::Lark => true,
    }
}

/// How a turn ended, from the live delivery's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LiveOutcome {
    Answer(String),
    Failure(String),
    Stopped,
}

pub(super) struct LiveTurn {
    pub(super) mode: ResolvedStream,
    locale: ImLocale,
    started_ms: i64,
    /// The answer text the deltas accumulated.
    text: String,
    /// The latest commentary or tool label, when the channel forwards those.
    progress: Option<String>,
    /// The text the provider last confirmed for the live message.
    shown: String,
    message_id: Option<String>,
    last_edit_ms: i64,
    finished: bool,
    outcome: Option<LiveOutcome>,
    watermark: i64,
    worker_running: bool,
    /// Whether the turn produced a message of its own.
    sent_any: bool,
    edits: u32,
}

impl LiveTurn {
    fn new(mode: ResolvedStream, locale: ImLocale, now_ms: i64) -> Self {
        Self {
            mode,
            locale,
            started_ms: now_ms,
            text: String::new(),
            progress: None,
            shown: String::new(),
            message_id: None,
            last_edit_ms: 0,
            finished: false,
            outcome: None,
            watermark: 0,
            worker_running: false,
            sent_any: false,
            edits: 0,
        }
    }

    /// Clears a finished turn so the next one starts from nothing.
    fn reset(&mut self, now_ms: i64) {
        let mode = self.mode;
        let locale = self.locale;
        *self = Self::new(mode, locale, now_ms);
    }

    /// What the live message should say right now, if anything.
    fn display(&self, now_ms: i64) -> Option<String> {
        let progress = || {
            let elapsed = now_ms.saturating_sub(self.started_ms).max(0);
            let bucket = elapsed / PROGRESS_CLOCK_BUCKET_MS * PROGRESS_CLOCK_BUCKET_MS / 1000;
            render::working(self.locale, self.progress.as_deref(), bucket)
        };
        match self.mode {
            ResolvedStream::Off | ResolvedStream::Typing => None,
            ResolvedStream::Progress => Some(progress()),
            ResolvedStream::Live => {
                if self.text.trim().is_empty() {
                    Some(progress())
                } else {
                    Some(render::live_answer(
                        &self.text,
                        LIVE_TAIL_CHARS,
                        self.locale,
                    ))
                }
            }
        }
    }
}

impl ImChannelService {
    /// The stream mode and platform stored for a channel.
    fn channel_stream_settings(&self, channel_id: &ImChannelId) -> (ImStreamMode, ImChannelKind) {
        let Some(conn) = self.open().ok() else {
            return (ImStreamMode::Auto, ImChannelKind::Telegram);
        };
        ImChannelRepository::get(&conn, channel_id)
            .ok()
            .flatten()
            .map(|record| {
                let mode = serde_json::from_str::<ImChannelConfigEnvelope>(&record.config_json)
                    .ok()
                    .map(|envelope| envelope.stream_mode)
                    .unwrap_or_default();
                (mode, record.kind)
            })
            .unwrap_or((ImStreamMode::Auto, ImChannelKind::Telegram))
    }

    /// The stream mode a binding's chat has asked for.
    pub(super) fn live_mode_for_binding(&self, binding: &ImBindingRecord) -> ResolvedStream {
        let (mode, kind) = self.channel_stream_settings(&binding.channel_id);
        resolve_stream(
            mode,
            None,
            group_hint(kind, &binding.external_chat_id, &binding.thread_key),
        )
    }

    /// The live turn for a binding, created on first use.
    fn live_slot(&self, binding: &ImBindingRecord) -> Arc<Mutex<LiveTurn>> {
        let mut turns = self.inner.live_turns.lock().expect("live turn registry");
        turns
            .entry(binding.binding_id.clone())
            .or_insert_with(|| {
                let mode = self.live_mode_for_binding(binding);
                let locale = self.locale_for_channel(&binding.channel_id);
                Arc::new(Mutex::new(LiveTurn::new(mode, locale, unix_timestamp_ms())))
            })
            .clone()
    }

    /// Records a streamed delta and makes sure a worker is drawing it.
    pub(super) fn note_live_delta(
        self: &Arc<Self>,
        binding: &ImBindingRecord,
        payload: &AgentMessageDeltaPayload,
    ) {
        let state = self.live_slot(binding);
        {
            let mut turn = state.lock().expect("live turn lock");
            if turn.finished {
                turn.reset(unix_timestamp_ms());
            }
            if !turn.mode.edits_a_message() {
                return;
            }
            match payload.phase {
                Some(AgentMessagePhase::Commentary) => {
                    let label = payload.text_delta.trim();
                    if !label.is_empty() {
                        turn.progress = Some(render::truncate(label, 120));
                    }
                }
                _ => turn.text.push_str(&payload.text_delta),
            }
        }
        self.ensure_live_worker(binding, &state);
    }

    /// Records a tool label for the progress line.
    ///
    /// Tool names ride the channel's event filter: a chat that turned tool
    /// progress off asked not to see them, and the progress line is not the
    /// place to overrule that.
    pub(super) fn note_live_progress(self: &Arc<Self>, binding: &ImBindingRecord, label: &str) {
        let allowed = self
            .open()
            .ok()
            .and_then(|conn| {
                ImChannelRepository::get(&conn, &binding.channel_id)
                    .ok()
                    .flatten()
            })
            .is_some_and(|record| event_allowed(&record, "tool_progress"));
        if !allowed || label.trim().is_empty() {
            return;
        }
        let state = self.live_slot(binding);
        {
            let mut turn = state.lock().expect("live turn lock");
            if turn.finished {
                turn.reset(unix_timestamp_ms());
            }
            if !turn.mode.edits_a_message() {
                return;
            }
            turn.progress = Some(render::truncate(label.trim(), 120));
        }
        self.ensure_live_worker(binding, &state);
    }

    /// Starts (or keeps) a live turn for a binding that is now running.
    ///
    /// The ticker cases need this even before any delta arrives: a `typing`
    /// action and a progress line both have something to say about a turn that
    /// has not produced text yet.
    pub(super) fn start_live_turn(
        self: &Arc<Self>,
        binding: &ImBindingRecord,
        chat_type: Option<ImChatType>,
    ) {
        let mode = {
            let (stored, kind) = self.channel_stream_settings(&binding.channel_id);
            resolve_stream(
                stored,
                chat_type,
                group_hint(kind, &binding.external_chat_id, &binding.thread_key),
            )
        };
        if mode == ResolvedStream::Off {
            return;
        }
        let state = self.live_slot(binding);
        {
            let mut turn = state.lock().expect("live turn lock");
            if turn.finished {
                turn.reset(unix_timestamp_ms());
            }
            // A binding that changed its stream setting picks the new mode up
            // on the next turn rather than mid-answer.
            turn.mode = mode;
        }
        self.ensure_live_worker(binding, &state);
    }

    fn ensure_live_worker(
        self: &Arc<Self>,
        binding: &ImBindingRecord,
        state: &Arc<Mutex<LiveTurn>>,
    ) {
        {
            let mut turn = state.lock().expect("live turn lock");
            if turn.worker_running {
                return;
            }
            turn.worker_running = true;
        }
        let service = Arc::downgrade(self);
        let binding = binding.clone();
        let state = Arc::clone(state);
        tokio::spawn(async move {
            let Some(service) = service.upgrade() else {
                return;
            };
            service.drive_live_turn(&binding, &state).await;
        });
    }

    /// Records how a turn ended. Returns whether the live path owns the reply.
    ///
    /// A channel in `Typing` mode only ever sent chat actions, so the answer
    /// still goes out through the ordinary path and the caller is told so.
    pub(super) async fn finish_live_turn(
        self: &Arc<Self>,
        binding: &ImBindingRecord,
        outcome: LiveOutcome,
        watermark: i64,
    ) -> bool {
        let state = {
            let turns = self.inner.live_turns.lock().expect("live turn registry");
            turns.get(&binding.binding_id).cloned()
        };
        let Some(state) = state else {
            return false;
        };
        let mode = {
            let mut turn = state.lock().expect("live turn lock");
            turn.finished = true;
            turn.outcome = Some(outcome);
            turn.watermark = watermark;
            turn.mode
        };
        if mode == ResolvedStream::Typing {
            self.await_live_worker(&state).await;
            self.forget_live_turn(&binding.binding_id);
            return false;
        }
        self.ensure_live_worker(binding, &state);
        self.await_live_worker(&state).await;
        self.forget_live_turn(&binding.binding_id);
        true
    }

    async fn await_live_worker(&self, state: &Arc<Mutex<LiveTurn>>) {
        let deadline = std::time::Instant::now() + LIVE_FINISH_TIMEOUT;
        loop {
            if std::time::Instant::now() >= deadline {
                tracing::warn!(target: "vibex_im", "IM live delivery did not finish");
                return;
            }
            let running = state
                .lock()
                .map(|turn| turn.worker_running)
                .unwrap_or(false);
            if !running {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    fn forget_live_turn(&self, binding_id: &ImBindingId) {
        if let Ok(mut turns) = self.inner.live_turns.lock() {
            turns.remove(binding_id);
        }
    }

    /// Stops a live turn whose turn was interrupted, leaving what it showed.
    pub(super) async fn stop_live_turn(self: &Arc<Self>, binding: &ImBindingRecord) {
        let has_turn = {
            let turns = self.inner.live_turns.lock().expect("live turn registry");
            turns.contains_key(&binding.binding_id)
        };
        if has_turn {
            let _ = self
                .finish_live_turn(binding, LiveOutcome::Stopped, 0)
                .await;
        }
    }

    async fn live_outbox(&self, channel_id: &ImChannelId) -> Option<Outbox> {
        self.inner
            .channels
            .read()
            .await
            .get(channel_id)
            .map(|runtime| runtime.outbox.clone())
    }

    fn live_target(binding: &ImBindingRecord) -> ImTarget {
        ImTarget {
            chat_id: binding.external_chat_id.clone(),
            thread_key: binding.thread_key.clone(),
        }
    }

    /// The worker owns every provider call for one live turn.
    async fn drive_live_turn(
        self: Arc<Self>,
        binding: &ImBindingRecord,
        state: &Arc<Mutex<LiveTurn>>,
    ) {
        let mut last_typing_ms = 0_i64;
        loop {
            if *self.inner.shutdown.borrow() {
                break;
            }
            let now = unix_timestamp_ms();
            let snapshot = {
                let turn = state.lock().expect("live turn lock");
                (
                    turn.mode,
                    turn.finished,
                    turn.outcome.clone(),
                    turn.display(now),
                    turn.message_id.clone(),
                    turn.shown.clone(),
                    turn.last_edit_ms,
                    turn.watermark,
                    turn.locale,
                    turn.edits,
                )
            };
            let (
                mode,
                finished,
                outcome,
                display,
                message_id,
                shown,
                last_edit_ms,
                watermark,
                locale,
                edits,
            ) = snapshot;

            if finished {
                // Only a mode that edits a message owns the final content.
                // `Typing` never showed one, so its outcome is left to the
                // ordinary send path or the answer would go out twice.
                if let Some(outcome) = outcome
                    && mode.edits_a_message()
                {
                    self.deliver_live_outcome(binding, state, &outcome, watermark, locale)
                        .await;
                }
                break;
            }

            if mode == ResolvedStream::Typing {
                // The tick count bounds a turn that never finishes on its own:
                // a chat action every few seconds is a hint, not a heartbeat
                // worth keeping for an hour.
                if now.saturating_sub(last_typing_ms) >= TYPING_TICK_MS && edits < 120 {
                    if let Some(outbox) = self.live_outbox(&binding.channel_id).await {
                        let _ = outbox
                            .activity(Self::live_target(binding), ImActivity::Typing)
                            .await;
                    }
                    last_typing_ms = now;
                    if let Ok(mut turn) = state.lock() {
                        turn.sent_any = true;
                        turn.edits = turn.edits.saturating_add(1);
                    }
                }
            } else if let Some(text) = display {
                let gap = mode.edit_gap_ms();
                if text != shown && now.saturating_sub(last_edit_ms) >= gap && edits < 120 {
                    let result = self
                        .draw_live_message(binding, message_id.as_deref(), &text)
                        .await;
                    let mut turn = state.lock().expect("live turn lock");
                    turn.edits = turn.edits.saturating_add(1);
                    match result {
                        Ok(settled) => {
                            turn.shown = text;
                            turn.last_edit_ms = unix_timestamp_ms();
                            if let Some(settled) = settled {
                                turn.message_id = Some(settled);
                            }
                            turn.sent_any = true;
                        }
                        Err(error) => {
                            // Back off on a refusal instead of hammering it.
                            // The final answer still goes out; only the
                            // progress line is lost.
                            turn.last_edit_ms = unix_timestamp_ms() + 3_000;
                            tracing::debug!(
                                target: "vibex_im",
                                error_code = %error.code(),
                                "IM live edit failed"
                            );
                        }
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(LIVE_POLL_MS)).await;
        }
        if let Ok(mut turn) = state.lock() {
            turn.worker_running = false;
        }
    }

    /// Sends or edits the one live message. Returns its id when it was created.
    async fn draw_live_message(
        &self,
        binding: &ImBindingRecord,
        message_id: Option<&str>,
        text: &str,
    ) -> ImResult<Option<String>> {
        let Some(outbox) = self.live_outbox(&binding.channel_id).await else {
            return Ok(None);
        };
        let target = Self::live_target(binding);
        match message_id {
            Some(message_id) => {
                outbox
                    .edit(
                        target,
                        message_id.to_string(),
                        text.to_string(),
                        true,
                        OutboundPriority::BestEffort,
                    )
                    .await?;
                Ok(None)
            }
            None => {
                let message_id = self.record_outbound(binding, text, "stream");
                let result = outbox
                    .send(
                        target,
                        text.to_string(),
                        Vec::new(),
                        true,
                        OutboundPriority::BestEffort,
                    )
                    .await;
                self.finish_outbound(&message_id, &result);
                let sent = result?;
                Ok(Some(sent.message_id))
            }
        }
    }

    /// Turns the recorded outcome into the chat's final message.
    async fn deliver_live_outcome(
        &self,
        binding: &ImBindingRecord,
        state: &Arc<Mutex<LiveTurn>>,
        outcome: &LiveOutcome,
        watermark: i64,
        locale: ImLocale,
    ) {
        let (message_id, partial, sent_any) = {
            let turn = state.lock().expect("live turn lock");
            (turn.message_id.clone(), turn.text.clone(), turn.sent_any)
        };
        let Some(outbox) = self.live_outbox(&binding.channel_id).await else {
            return;
        };
        let target = Self::live_target(binding);

        let delivered = match outcome {
            LiveOutcome::Answer(answer) => {
                let parts = render::final_answer_parts(answer, locale);
                let mut ok = true;
                for (index, part) in parts.iter().enumerate() {
                    let result = if index == 0
                        && let Some(message_id) = message_id.as_deref()
                    {
                        outbox
                            .edit(
                                target.clone(),
                                message_id.to_string(),
                                part.clone(),
                                true,
                                OutboundPriority::Normal,
                            )
                            .await
                    } else {
                        let record = self.record_outbound(binding, part, "message");
                        let result = outbox
                            .send(
                                target.clone(),
                                part.clone(),
                                Vec::new(),
                                true,
                                OutboundPriority::Normal,
                            )
                            .await;
                        self.finish_outbound(&record, &result);
                        result.map(|_| ())
                    };
                    if result.is_err() {
                        ok = false;
                        break;
                    }
                }
                ok
            }
            LiveOutcome::Failure(text) => {
                let body = if partial.trim().is_empty() {
                    text.clone()
                } else {
                    format!(
                        "{}\n\n{}",
                        render::truncate(&partial, LIVE_TAIL_CHARS),
                        text
                    )
                };
                self.draw_final_line(
                    binding,
                    &outbox,
                    &target,
                    message_id.as_deref(),
                    &body,
                    false,
                )
                .await
            }
            LiveOutcome::Stopped => {
                let label = render::stopped(locale);
                let body = if partial.trim().is_empty() {
                    label.to_string()
                } else {
                    format!("{}\n\n{label}", render::truncate(&partial, LIVE_TAIL_CHARS))
                };
                // An interrupted turn with no message of its own says nothing:
                // a chat does not need a notice for a turn it never saw.
                if message_id.is_none() && !sent_any {
                    true
                } else {
                    self.draw_final_line(
                        binding,
                        &outbox,
                        &target,
                        message_id.as_deref(),
                        &body,
                        false,
                    )
                    .await
                }
            }
        };

        if delivered
            && watermark > 0
            && let Ok(conn) = self.open()
        {
            let _ = ImBindingRepository::set_delivered_sequence(
                &conn,
                &binding.binding_id,
                watermark,
                unix_timestamp_ms(),
            );
        }
    }

    async fn draw_final_line(
        &self,
        binding: &ImBindingRecord,
        outbox: &Outbox,
        target: &ImTarget,
        message_id: Option<&str>,
        body: &str,
        markdown: bool,
    ) -> bool {
        match message_id {
            Some(message_id) => outbox
                .edit(
                    target.clone(),
                    message_id.to_string(),
                    body.to_string(),
                    markdown,
                    OutboundPriority::Normal,
                )
                .await
                .is_ok(),
            None => {
                let record = self.record_outbound(binding, body, "message");
                let result = outbox
                    .send(
                        target.clone(),
                        body.to_string(),
                        Vec::new(),
                        markdown,
                        OutboundPriority::Normal,
                    )
                    .await;
                self.finish_outbound(&record, &result);
                result.is_ok()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_streams_a_direct_message_and_settles_a_group() {
        assert_eq!(
            resolve_stream(ImStreamMode::Auto, Some(ImChatType::Direct), true),
            ResolvedStream::Live
        );
        assert_eq!(
            resolve_stream(ImStreamMode::Auto, Some(ImChatType::Group), false),
            ResolvedStream::Progress
        );
    }

    #[test]
    fn auto_reads_an_unknown_chat_from_its_transport_shape() {
        // A timeline event names the binding, not the inbound message.
        assert_eq!(
            resolve_stream(
                ImStreamMode::Auto,
                None,
                group_hint(vibex_core::ImChannelKind::Telegram, "-100123", "")
            ),
            ResolvedStream::Progress
        );
        assert_eq!(
            resolve_stream(
                ImStreamMode::Auto,
                None,
                group_hint(vibex_core::ImChannelKind::Telegram, "42", "")
            ),
            ResolvedStream::Live
        );
        assert_eq!(
            resolve_stream(
                ImStreamMode::Auto,
                None,
                group_hint(vibex_core::ImChannelKind::Lark, "oc_chat", "")
            ),
            ResolvedStream::Progress
        );
    }

    #[test]
    fn an_explicit_mode_wins_over_the_chat_shape() {
        assert_eq!(
            resolve_stream(ImStreamMode::Off, Some(ImChatType::Direct), false),
            ResolvedStream::Off
        );
        assert_eq!(
            resolve_stream(ImStreamMode::Live, Some(ImChatType::Group), true),
            ResolvedStream::Live
        );
    }

    #[test]
    fn a_live_display_keeps_the_tail_of_a_long_answer() {
        let mut turn = LiveTurn::new(ResolvedStream::Live, ImLocale::En, 0);
        turn.text = "x".repeat(10_000);
        let display = turn.display(1_000).expect("display");
        assert!(display.chars().count() < 10_000);
        assert!(
            display.starts_with('…') || display.ends_with('▍'),
            "{display}"
        );
    }
}

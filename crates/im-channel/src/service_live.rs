//! Live turn delivery: one progress line and the answer as it grows.
//!
//! A chat sees each assistant text block as its own message, edited while that
//! block streams, and one progress message that is edited as tools and thinking
//! change. A turn that writes, calls a tool, and writes again therefore leaves
//! several readable messages behind instead of one message that keeps mutating.
//!
//! The worker is the only writer. The event loop never blocks on an edit; it
//! records what the turn produced and makes sure a worker is running. That
//! split is what keeps a slow provider call from stalling every other chat on
//! the same timeline subscription, and it is why the edits can be coalesced:
//! the worker redraws from the latest state, so a burst of deltas costs one
//! edit instead of one edit each.

use vibex_core::{AgentMessageDeltaPayload, AgentMessagePhase};

use super::*;
use crate::error::ImResult;
use crate::types::ImTarget;

/// How long a streaming text message waits between edits.
///
/// A person does not read faster than this, and every edit is a provider call
/// that competes with approvals and final answers for the same quota.
const TEXT_EDIT_GAP_MS: i64 = 1_500;
/// How long a progress line waits between edits. It changes less often than an
/// answer grows, so it can afford to be thriftier.
const PROGRESS_EDIT_GAP_MS: i64 = 2_500;
/// How often a progress line's clock is redrawn.
const PROGRESS_CLOCK_BUCKET_MS: i64 = 5_000;
/// How often the worker wakes to reconsider the message.
const LIVE_POLL_MS: u64 = 400;
/// The most edits one turn will make, so a runaway stream stops spending the
/// provider's quota on itself.
const MAX_TURN_EDITS: u32 = 200;
/// How long a finalizer waits for the worker to deliver before giving up.
const LIVE_FINISH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// How a turn ended, from the live delivery's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LiveOutcome {
    Answer(String),
    Failure(String),
    Stopped,
}

/// One assistant text block, streamed into its own chat message.
#[derive(Debug, Clone, Default)]
struct TextRun {
    /// The provider message this block arrived under, when it has one. A new
    /// correlation starts a new chat message even without an activity between.
    correlation: Option<String>,
    /// The provider's phase, when it reports one. A phase change is a boundary.
    phase: Option<AgentMessagePhase>,
    text: String,
    message_id: Option<String>,
    shown: String,
    last_edit_ms: i64,
    /// A block that has ended: it is drawn but never appended to again.
    sealed: bool,
}

/// The turn's single activity message.
#[derive(Debug, Clone, Default)]
struct ProgressRun {
    message_id: Option<String>,
    shown: String,
    label: Option<String>,
    last_edit_ms: i64,
}

/// One redraw the worker owes the chat.
enum LiveDraw {
    Text {
        index: usize,
        message_id: Option<String>,
        text: String,
    },
    Progress {
        message_id: Option<String>,
        text: String,
    },
}

pub(super) struct LiveTurn {
    locale: ImLocale,
    started_ms: i64,
    /// Every text block of the turn, in order; the last one is still growing.
    runs: Vec<TextRun>,
    progress: Option<ProgressRun>,
    finished: bool,
    outcome: Option<LiveOutcome>,
    watermark: i64,
    worker_running: bool,
    edits: u32,
}

impl LiveTurn {
    fn new(locale: ImLocale, now_ms: i64) -> Self {
        Self {
            locale,
            started_ms: now_ms,
            runs: Vec::new(),
            progress: None,
            finished: false,
            outcome: None,
            watermark: 0,
            worker_running: false,
            edits: 0,
        }
    }

    /// Clears a finished turn so the next one starts from nothing.
    fn reset(&mut self, now_ms: i64) {
        let locale = self.locale;
        *self = Self::new(locale, now_ms);
    }

    /// Ends the block that is streaming, so the next delta opens a new message.
    fn seal_current(&mut self) {
        if let Some(run) = self.runs.last_mut() {
            run.sealed = true;
        }
    }

    /// Records one delta, starting a new chat message at a block boundary.
    fn append_delta(&mut self, delta: &AgentMessageDeltaPayload, correlation: Option<&str>) {
        if delta.text_delta.is_empty() {
            return;
        }
        let boundary = match self.runs.last() {
            None => true,
            Some(run) => {
                run.sealed
                    || (correlation.is_some()
                        && run.correlation.is_some()
                        && run.correlation.as_deref() != correlation)
                    || (delta.phase.is_some() && run.phase.is_some() && run.phase != delta.phase)
            }
        };
        if boundary {
            self.runs.push(TextRun {
                correlation: correlation.map(str::to_string),
                phase: delta.phase,
                ..TextRun::default()
            });
        }
        if let Some(run) = self.runs.last_mut() {
            if run.correlation.is_none() {
                run.correlation = correlation.map(str::to_string);
            }
            if run.phase.is_none() {
                run.phase = delta.phase;
            }
            run.text.push_str(&delta.text_delta);
        }
    }

    fn note_activity(&mut self, label: Option<String>) {
        self.seal_current();
        let progress = self.progress.get_or_insert_with(ProgressRun::default);
        if label.is_some() {
            progress.label = label;
        }
    }

    /// Shows the progress line from the start of the turn, before the first
    /// tool or thought names what the turn is doing.
    fn begin_activity(&mut self) {
        self.progress.get_or_insert_with(ProgressRun::default);
    }

    /// The progress line's text right now, if the turn has shown activity.
    fn display_progress(&self, now_ms: i64) -> Option<String> {
        let progress = self.progress.as_ref()?;
        let elapsed = now_ms.saturating_sub(self.started_ms).max(0);
        let bucket = elapsed / PROGRESS_CLOCK_BUCKET_MS * PROGRESS_CLOCK_BUCKET_MS / 1000;
        Some(render::working(
            self.locale,
            progress.label.as_deref(),
            bucket,
        ))
    }
}

/// The next redraw the worker owes, if any.
fn pending_draw(state: &Arc<Mutex<LiveTurn>>, now_ms: i64, ignore_gap: bool) -> Option<LiveDraw> {
    let turn = state.lock().expect("live turn lock");
    if !ignore_gap && turn.edits >= MAX_TURN_EDITS {
        return None;
    }
    for (index, run) in turn.runs.iter().enumerate() {
        if run.text.trim().is_empty() {
            continue;
        }
        let text = render::live_text(&run.text);
        if text == run.shown {
            continue;
        }
        if !ignore_gap && now_ms.saturating_sub(run.last_edit_ms) < TEXT_EDIT_GAP_MS {
            continue;
        }
        return Some(LiveDraw::Text {
            index,
            message_id: run.message_id.clone(),
            text,
        });
    }
    let progress = turn.progress.as_ref()?;
    let text = turn.display_progress(now_ms)?;
    if text == progress.shown {
        return None;
    }
    if !ignore_gap && now_ms.saturating_sub(progress.last_edit_ms) < PROGRESS_EDIT_GAP_MS {
        return None;
    }
    Some(LiveDraw::Progress {
        message_id: progress.message_id.clone(),
        text,
    })
}

impl ImChannelService {
    /// The live turn for a binding, created on first use.
    fn live_slot(&self, binding: &ImBindingRecord) -> Arc<Mutex<LiveTurn>> {
        let mut turns = self.inner.live_turns.lock().expect("live turn registry");
        turns
            .entry(binding.binding_id.clone())
            .or_insert_with(|| {
                let locale = self.locale_for_channel(&binding.channel_id);
                Arc::new(Mutex::new(LiveTurn::new(locale, unix_timestamp_ms())))
            })
            .clone()
    }

    /// Records a streamed delta and makes sure a worker is drawing it.
    ///
    /// `correlation` is the provider's message id: a change means a new text
    /// block, which becomes a new chat message.
    pub(super) fn note_live_delta(
        self: &Arc<Self>,
        binding: &ImBindingRecord,
        payload: &AgentMessageDeltaPayload,
        correlation: Option<&str>,
    ) {
        let state = self.live_slot(binding);
        {
            let mut turn = state.lock().expect("live turn lock");
            if turn.finished {
                turn.reset(unix_timestamp_ms());
            }
            turn.append_delta(payload, correlation);
        }
        self.ensure_live_worker(binding, &state);
    }

    /// Records a tool call as the turn's current activity.
    pub(super) fn note_live_tool(
        self: &Arc<Self>,
        binding: &ImBindingRecord,
        tool_name: &str,
        summary: &str,
    ) {
        let locale = self.locale_for_channel(&binding.channel_id);
        let label = render::activity(locale, tool_name, summary);
        self.note_live_activity(binding, Some(label));
    }

    /// Records model reasoning as the turn's current activity.
    pub(super) fn note_live_thinking(self: &Arc<Self>, binding: &ImBindingRecord) {
        let locale = self.locale_for_channel(&binding.channel_id);
        let label = render::thinking(locale).to_string();
        self.note_live_activity(binding, Some(label));
    }

    fn note_live_activity(self: &Arc<Self>, binding: &ImBindingRecord, label: Option<String>) {
        let state = self.live_slot(binding);
        {
            let mut turn = state.lock().expect("live turn lock");
            if turn.finished {
                turn.reset(unix_timestamp_ms());
            }
            turn.note_activity(label);
        }
        self.ensure_live_worker(binding, &state);
    }

    /// Starts (or keeps) a live turn for a binding that is now running.
    pub(super) fn start_live_turn(self: &Arc<Self>, binding: &ImBindingRecord) {
        let state = self.live_slot(binding);
        {
            let mut turn = state.lock().expect("live turn lock");
            if turn.finished {
                turn.reset(unix_timestamp_ms());
            }
            turn.begin_activity();
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
        {
            let mut turn = state.lock().expect("live turn lock");
            turn.finished = true;
            turn.outcome = Some(outcome);
            turn.watermark = watermark;
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
        loop {
            if *self.inner.shutdown.borrow() {
                break;
            }
            let now = unix_timestamp_ms();
            let (finished, outcome, watermark, locale) = {
                let turn = state.lock().expect("live turn lock");
                (
                    turn.finished,
                    turn.outcome.clone(),
                    turn.watermark,
                    turn.locale,
                )
            };
            if finished {
                // Everything the turn produced still has to reach the chat
                // before the outcome replaces or follows it.
                self.flush_live_turn(binding, state).await;
                if let Some(outcome) = outcome {
                    self.deliver_live_outcome(binding, state, &outcome, watermark, locale)
                        .await;
                }
                break;
            }
            if let Some(draw) = pending_draw(state, now, false) {
                self.draw_live(binding, state, draw).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(LIVE_POLL_MS)).await;
        }
        if let Ok(mut turn) = state.lock() {
            turn.worker_running = false;
        }
    }

    /// Draws everything still outstanding, ignoring the coalescing gaps.
    async fn flush_live_turn(
        self: &Arc<Self>,
        binding: &ImBindingRecord,
        state: &Arc<Mutex<LiveTurn>>,
    ) {
        for _ in 0..8 {
            let Some(draw) = pending_draw(state, unix_timestamp_ms(), true) else {
                break;
            };
            self.draw_live(binding, state, draw).await;
        }
    }

    async fn draw_live(
        self: &Arc<Self>,
        binding: &ImBindingRecord,
        state: &Arc<Mutex<LiveTurn>>,
        draw: LiveDraw,
    ) {
        let Some(outbox) = self.live_outbox(&binding.channel_id).await else {
            return;
        };
        match draw {
            LiveDraw::Text {
                index,
                message_id,
                text,
            } => {
                let result = self
                    .draw_live_message(binding, &outbox, message_id.as_deref(), &text)
                    .await;
                let mut turn = state.lock().expect("live turn lock");
                turn.edits = turn.edits.saturating_add(1);
                let Some(run) = turn.runs.get_mut(index) else {
                    return;
                };
                match result {
                    Ok(settled) => {
                        run.shown = text;
                        run.last_edit_ms = unix_timestamp_ms();
                        if settled.is_some() {
                            run.message_id = settled;
                        }
                    }
                    Err(error) => {
                        // Back off on a refusal instead of hammering it. The
                        // final answer still goes out; only the live text is lost.
                        run.last_edit_ms = unix_timestamp_ms() + 3_000;
                        tracing::debug!(
                            target: "vibex_im",
                            error_code = %error.code(),
                            "IM live text edit failed"
                        );
                    }
                }
            }
            LiveDraw::Progress { message_id, text } => {
                let result = self
                    .draw_live_message(binding, &outbox, message_id.as_deref(), &text)
                    .await;
                let mut turn = state.lock().expect("live turn lock");
                turn.edits = turn.edits.saturating_add(1);
                let Some(progress) = turn.progress.as_mut() else {
                    return;
                };
                match result {
                    Ok(settled) => {
                        progress.shown = text;
                        progress.last_edit_ms = unix_timestamp_ms();
                        if settled.is_some() {
                            progress.message_id = settled;
                        }
                    }
                    Err(error) => {
                        progress.last_edit_ms = unix_timestamp_ms() + 3_000;
                        tracing::debug!(
                            target: "vibex_im",
                            error_code = %error.code(),
                            "IM progress edit failed"
                        );
                    }
                }
            }
        }
    }

    /// Sends or edits the one live message. Returns its id when it was created.
    async fn draw_live_message(
        &self,
        binding: &ImBindingRecord,
        outbox: &Outbox,
        message_id: Option<&str>,
        text: &str,
    ) -> ImResult<Option<String>> {
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
        let (runs, progress_id) = {
            let turn = state.lock().expect("live turn lock");
            (
                turn.runs.clone(),
                turn.progress
                    .as_ref()
                    .and_then(|progress| progress.message_id.clone()),
            )
        };
        let Some(outbox) = self.live_outbox(&binding.channel_id).await else {
            return;
        };
        let target = Self::live_target(binding);

        let delivered = match outcome {
            LiveOutcome::Answer(answer) => {
                // Blocks already closed are in the chat; the final text repeats
                // them, so only its own tail is news.
                let closed: String = runs
                    .iter()
                    .take(runs.len().saturating_sub(1))
                    .map(|run| run.text.as_str())
                    .collect();
                let remainder = answer.strip_prefix(closed.as_str()).unwrap_or(answer);
                let parts = render::final_answer_parts(remainder, locale);
                let mut message_id = runs.last().and_then(|run| run.message_id.clone());
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
                    message_id = None;
                }
                ok
            }
            LiveOutcome::Failure(text) => {
                self.draw_final_line(
                    binding,
                    &outbox,
                    &target,
                    progress_id.as_deref(),
                    text,
                    false,
                )
                .await
            }
            LiveOutcome::Stopped => {
                let label = render::stopped(locale);
                let any_shown = runs.iter().any(|run| run.message_id.is_some());
                if progress_id.is_none() && !any_shown {
                    // A chat does not need a notice for a turn it never saw.
                    true
                } else {
                    self.draw_final_line(
                        binding,
                        &outbox,
                        &target,
                        progress_id.as_deref(),
                        label,
                        false,
                    )
                    .await
                }
            }
        };

        // The activity line outlives the work it described; settle it once the
        // answer has landed rather than leaving a timer running forever.
        if matches!(outcome, LiveOutcome::Answer(_))
            && let Some(progress_id) = progress_id.as_deref()
        {
            let label = render::finished(locale);
            let _ = outbox
                .edit(
                    target,
                    progress_id.to_string(),
                    label.to_string(),
                    true,
                    OutboundPriority::Normal,
                )
                .await;
        }

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

    fn delta(text: &str, phase: Option<AgentMessagePhase>) -> AgentMessageDeltaPayload {
        AgentMessageDeltaPayload {
            text_delta: text.to_string(),
            chunk_index: 0,
            phase,
        }
    }

    #[test]
    fn a_new_provider_message_starts_a_new_chat_message() {
        let mut turn = LiveTurn::new(ImLocale::En, 0);
        turn.append_delta(&delta("first", None), Some("m1"));
        turn.append_delta(&delta(" second", None), Some("m1"));
        turn.append_delta(&delta("other", None), Some("m2"));
        assert_eq!(turn.runs.len(), 2);
        assert_eq!(turn.runs[0].text, "first second");
        assert_eq!(turn.runs[1].text, "other");
    }

    #[test]
    fn a_phase_change_starts_a_new_chat_message() {
        let mut turn = LiveTurn::new(ImLocale::En, 0);
        turn.append_delta(
            &delta("thinking aloud", Some(AgentMessagePhase::Commentary)),
            None,
        );
        turn.append_delta(
            &delta("the answer", Some(AgentMessagePhase::FinalAnswer)),
            None,
        );
        assert_eq!(turn.runs.len(), 2);
        assert_eq!(turn.runs[0].text, "thinking aloud");
        assert_eq!(turn.runs[1].text, "the answer");
    }

    #[test]
    fn an_activity_seals_the_block_so_later_text_is_a_new_message() {
        let mut turn = LiveTurn::new(ImLocale::En, 0);
        turn.append_delta(&delta("before the tool", None), None);
        turn.note_activity(Some("Running a command".to_string()));
        turn.append_delta(&delta("after the tool", None), None);
        assert_eq!(turn.runs.len(), 2);
        assert_eq!(turn.runs[0].text, "before the tool");
        assert_eq!(turn.runs[1].text, "after the tool");
        assert!(turn.progress.is_some());
    }

    #[test]
    fn the_live_text_is_bounded_and_carries_no_cursor() {
        let text = "x".repeat(10_000);
        let shown = render::live_text(&text);
        assert!(shown.chars().count() <= 3_800);
        assert!(!shown.contains('▍'));
    }
}

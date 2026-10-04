//! What the running turn is doing, and how fast.
//!
//! The desktop draws one line above its composer while a session runs: the
//! phase the Agent is in, how long the turn has taken, how many tools it has
//! called and how fast it is writing. The TUI draws the same line above its
//! prompt, and this module is where its answers come from.
//!
//! Every reading here is taken from the projection the client already holds —
//! the conversation turns built by `vibex-desktop-model` from the timeline — so
//! nothing in this module asks the runtime a question of its own. The one
//! number no event reports is the output rate: the TUI has no token counter to
//! read, so it estimates one from the characters the turn has streamed, which
//! is the same fallback the desktop uses when its counter is silent.

use std::time::{Duration, Instant};

use vibex_desktop_model::{TimelineConversationTurn, TimelineRowKind};

/// Which part of a turn the Agent is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnPhase {
    /// The turn is accepted and nothing has arrived yet.
    Preparing,
    /// The Agent is reasoning, or is busy without saying at what.
    Thinking,
    /// The Agent is waiting on a tool.
    CallingTool,
    /// The Agent is writing its answer.
    Generating,
    /// The turn stopped to ask the reader for permission.
    WaitingForApproval,
}

/// The phase a turn is in, read from what it is still streaming.
///
/// The newest row that is still arriving is what the Agent is doing: an open
/// tool row means a tool is still running, an answer still arriving means the
/// Agent is writing. A turn with nothing streaming is quieter than that — it is
/// waiting on the reader, or it has not started producing — and the runtime's
/// own live line is the only thing left that can tell thinking from nothing.
pub fn phase(turn: &TimelineConversationTurn) -> TurnPhase {
    if turn.pending_permission {
        return TurnPhase::WaitingForApproval;
    }
    // The conclusion is checked first because it is the newest row: a turn that
    // has started its answer is generating even while its last tool row sits
    // above it.
    let streaming = turn
        .conclusion_row
        .iter()
        .chain(turn.process_rows.iter().rev())
        .find(|row| row.streaming);
    match streaming.map(|row| row.kind) {
        Some(TimelineRowKind::AgentMessage) => TurnPhase::Generating,
        Some(
            TimelineRowKind::ToolCall
            | TimelineRowKind::Command
            | TimelineRowKind::FileOperation
            | TimelineRowKind::WebSearch
            | TimelineRowKind::Collaboration
            | TimelineRowKind::ImageGeneration,
        ) => TurnPhase::CallingTool,
        Some(TimelineRowKind::Reasoning) => TurnPhase::Thinking,
        // The runtime's own live line is the only evidence left: a turn that
        // says something is thinking, and one that says nothing has not begun.
        Some(_) | None if turn.live_status.is_some() => TurnPhase::Thinking,
        Some(_) | None => TurnPhase::Preparing,
    }
}

/// How many tools the turn has called so far.
///
/// One row is one call: the projection folds a call's arguments and its result
/// into a single row, so counting rows is counting calls rather than the two
/// events each call produces.
pub fn tool_call_count(turn: &TimelineConversationTurn) -> usize {
    turn.process_rows
        .iter()
        .filter(|row| {
            matches!(
                row.kind,
                TimelineRowKind::ToolCall
                    | TimelineRowKind::Command
                    | TimelineRowKind::FileOperation
                    | TimelineRowKind::WebSearch
                    | TimelineRowKind::Collaboration
                    | TimelineRowKind::ImageGeneration
            )
        })
        .count()
}

/// What the turn has produced so far, in estimated tokens.
///
/// The client has no token counter, so the estimate is a quarter of the
/// characters the turn has streamed — the same ratio the desktop falls back to.
/// Reasoning counts as output because the model wrote it, and the live status
/// line counts only when no reasoning row already carries it, or a turn that
/// streams its reasoning through `live_status` would be counted twice.
pub fn generated_tokens(turn: &TimelineConversationTurn) -> Option<u64> {
    let mut streaming_reasoning_row = false;
    let mut characters = 0usize;
    for row in turn.process_rows.iter().chain(turn.conclusion_row.iter()) {
        match row.kind {
            TimelineRowKind::AgentMessage => characters += row.body.chars().count(),
            TimelineRowKind::Reasoning => {
                streaming_reasoning_row |= row.streaming;
                characters += row.body.chars().count();
            }
            _ => {}
        }
    }
    if !streaming_reasoning_row && let Some(status) = turn.live_status.as_deref() {
        characters += status.chars().count();
    }
    (characters > 0).then_some((characters as u64).saturating_add(3) / 4)
}

/// The rate two observations of a turn add up to.
///
/// Only forward movement has a rate: a turn that produced nothing, or one whose
/// projection shrank because a row was replaced, has no speed to report.
pub fn token_rate(
    previous_tokens: Option<u64>,
    current_tokens: Option<u64>,
    elapsed: Duration,
) -> Option<f32> {
    let (Some(previous), Some(current)) = (previous_tokens, current_tokens) else {
        return None;
    };
    let elapsed_seconds = elapsed.as_secs_f32();
    // `then` rather than `then_some`: the difference must not be computed for a
    // turn that went backwards, which is what a replaced row looks like.
    (current > previous && elapsed_seconds > 0.0)
        .then(|| (current - previous) as f32 / elapsed_seconds)
}

/// The client's reading of the live turn: what the line above the composer is
/// made of.
///
/// The phase and the tool count are properties of the projection and are
/// re-read on every observation. The rate is a difference between two
/// observations, so it needs the previous one — which is why the reading is
/// held here rather than derived at draw time. A new turn starts the sample
/// over: the last turn's speed says nothing about this one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnReadout {
    phase: Option<TurnPhase>,
    tool_calls: usize,
    tokens_per_second: Option<f32>,
    turn_id: Option<String>,
    generated_tokens: Option<u64>,
    sampled_at: Option<Instant>,
}

impl TurnReadout {
    /// Fold one observation of the live turn in, answering whether the line
    /// above the composer would change.
    pub fn observe(&mut self, turn: &TimelineConversationTurn, now: Instant) -> bool {
        let phase = phase(turn);
        let tool_calls = tool_call_count(turn);
        let current_tokens = generated_tokens(turn);
        let mut changed = self.phase != Some(phase) || self.tool_calls != tool_calls;
        self.phase = Some(phase);
        self.tool_calls = tool_calls;

        if self.turn_id.as_deref() != Some(turn.id.as_str()) {
            changed |= self.tokens_per_second.take().is_some();
            self.turn_id = Some(turn.id.clone());
            self.generated_tokens = current_tokens;
            self.sampled_at = Some(now);
            return changed;
        }
        if current_tokens == self.generated_tokens {
            return changed;
        }
        // The sample advances only when the output did: a rate measured against
        // a stretch that produced nothing would report the silence as slowness.
        let elapsed = self.sampled_at.map_or(Duration::ZERO, |sampled| {
            now.saturating_duration_since(sampled)
        });
        if let Some(rate) = token_rate(self.generated_tokens, current_tokens, elapsed) {
            changed |= self.tokens_per_second != Some(rate);
            self.tokens_per_second = Some(rate);
        }
        self.generated_tokens = current_tokens;
        self.sampled_at = Some(now);
        changed
    }

    /// Forget the turn: nothing is running, and a stale phase is a claim about
    /// a turn that has ended.
    pub fn forget(&mut self) -> bool {
        let changed = *self != Self::default();
        *self = Self::default();
        changed
    }

    pub const fn phase(&self) -> Option<TurnPhase> {
        self.phase
    }

    pub const fn tool_calls(&self) -> usize {
        self.tool_calls
    }

    pub const fn tokens_per_second(&self) -> Option<f32> {
        self.tokens_per_second
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_desktop_model::TimelineRow;

    fn row(kind: TimelineRowKind, streaming: bool) -> TimelineRow {
        TimelineRow {
            id: format!("row-{kind:?}"),
            kind,
            item_ids: Vec::new(),
            turn_id: Some("turn-1".into()),
            turn_item_count: 1,
            turn_failed: false,
            turn_pending_permission: false,
            conclusion: false,
            first_sequence: 1,
            last_sequence: 1,
            title: String::new(),
            body: String::new(),
            streaming,
            collapsible: false,
            pending_permission: false,
            failed: false,
            runtime_attribution: None,
            file_path: None,
        }
    }

    fn turn(id: &str) -> TimelineConversationTurn {
        TimelineConversationTurn {
            id: id.into(),
            user_row: None,
            process_rows: Vec::new(),
            process_activity_groups: Vec::new(),
            process_activity_groups_with_commands: Vec::new(),
            process_activity_groups_with_file_operations: Vec::new(),
            process_activity_groups_with_commands_and_file_operations: Vec::new(),
            live_status: None,
            conclusion_row: None,
            runtime_attribution: None,
            complete: false,
            superseded: false,
            failed: false,
            pending_permission: false,
            item_count: 0,
            started_at_ms: 0,
            ended_at_ms: None,
        }
    }

    #[test]
    fn a_turn_with_nothing_streaming_is_only_preparing() {
        assert_eq!(phase(&turn("a")), TurnPhase::Preparing);
    }

    #[test]
    fn the_newest_streaming_row_names_the_phase() {
        let mut turn = turn("a");
        turn.process_rows.push(row(TimelineRowKind::ToolCall, true));
        assert_eq!(phase(&turn), TurnPhase::CallingTool);

        turn.process_rows
            .push(row(TimelineRowKind::Reasoning, true));
        assert_eq!(phase(&turn), TurnPhase::Thinking);

        // The conclusion is the newest row, so an answer being written outranks
        // the tool row that came before it.
        turn.conclusion_row = Some(row(TimelineRowKind::AgentMessage, true));
        assert_eq!(phase(&turn), TurnPhase::Generating);
    }

    #[test]
    fn a_turn_waiting_on_the_reader_says_so() {
        let mut turn = turn("a");
        turn.process_rows.push(row(TimelineRowKind::ToolCall, true));
        turn.pending_permission = true;
        assert_eq!(phase(&turn), TurnPhase::WaitingForApproval);
    }

    #[test]
    fn only_tool_rows_are_counted_as_calls() {
        let mut turn = turn("a");
        turn.process_rows
            .push(row(TimelineRowKind::ToolCall, false));
        turn.process_rows
            .push(row(TimelineRowKind::AgentMessage, false));
        turn.process_rows
            .push(row(TimelineRowKind::FileOperation, false));
        assert_eq!(tool_call_count(&turn), 2);
    }

    #[test]
    fn the_estimate_counts_reasoning_once() {
        let mut turn = turn("a");
        let mut reasoning = row(TimelineRowKind::Reasoning, true);
        reasoning.body = "12345678".into();
        turn.process_rows.push(reasoning);
        // The live line carries the same stream, so it is not counted again.
        turn.live_status = Some("1234".into());
        assert_eq!(generated_tokens(&turn), Some(2));
    }

    #[test]
    fn a_rate_needs_two_forward_numbers() {
        let two_seconds = Duration::from_secs(2);
        assert_eq!(token_rate(Some(10), Some(30), two_seconds), Some(10.0));
        assert_eq!(token_rate(Some(30), Some(30), two_seconds), None);
        assert_eq!(token_rate(Some(30), Some(10), two_seconds), None);
        assert_eq!(token_rate(None, Some(30), two_seconds), None);
        assert_eq!(token_rate(Some(10), None, two_seconds), None);
    }

    #[test]
    fn a_readout_forgets_the_speed_of_the_turn_before_it() {
        let start = Instant::now();
        let mut readout = TurnReadout::default();

        let mut first = turn("turn-1");
        let mut message = row(TimelineRowKind::AgentMessage, true);
        message.body = "aaaaaaaa".repeat(4);
        first.conclusion_row = Some(message.clone());
        assert!(readout.observe(&first, start));
        assert_eq!(readout.phase(), Some(TurnPhase::Generating));
        assert_eq!(readout.tokens_per_second(), None);

        // Eight more characters is two estimated tokens over one second.
        let mut grown = first.clone();
        grown.conclusion_row = Some(TimelineRow {
            body: format!("{}{}", message.body, "bbbbbbbb"),
            ..message.clone()
        });
        assert!(readout.observe(&grown, start + Duration::from_secs(1)));
        assert_eq!(readout.tokens_per_second(), Some(2.0));

        // The next turn starts the sample over rather than inheriting it.
        let second = turn("turn-2");
        assert!(readout.observe(&second, start + Duration::from_secs(2)));
        assert_eq!(readout.tokens_per_second(), None);
        assert_eq!(readout.phase(), Some(TurnPhase::Preparing));
    }

    #[test]
    fn a_forgotten_readout_has_nothing_to_say() {
        let mut readout = TurnReadout::default();
        assert!(!readout.forget());
        readout.observe(&turn("turn-1"), Instant::now());
        assert!(readout.forget());
        assert_eq!(readout.phase(), None);
        assert_eq!(readout.tool_calls(), 0);
        assert_eq!(readout.tokens_per_second(), None);
    }
}

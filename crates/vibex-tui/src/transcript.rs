//! The transcript: block-level caching, incremental layout, and viewport-only
//! rendering.
//!
//! This is where an Agent terminal lives or dies. The naive implementation —
//! re-wrapping the whole conversation every frame — is unusable on a long
//! session, so the module is built around four rules:
//!
//! 1. **A block caches its rendered lines and its height.** Re-wrapping only
//!    happens when a block actually changes.
//! 2. **Only dirty blocks are re-measured.** Appending a streamed token
//!    dirties exactly one block.
//! 3. **Streaming updates rewrite the last block in place.**
//! 4. **Blocks outside the viewport are estimated, not rendered.** Their exact
//!    height is measured the first time the viewport reaches them, and their
//!    styled lines are only materialised for the visible window.
//!
//! The resulting contract is that an idle transcript costs zero frames, and
//! scrolling a 10 000-block conversation does not depend on how much history
//! sits above the viewport.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    ops::Range,
    time::{Duration, Instant},
};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use vibex_desktop_model::{ReasoningExpansionMode, TimelineRowKind};
use vibex_ui::timeline::{Activity, ActivitySummary};
use vibex_ui::tool_detail::{self, Detail};

use crate::locale::Strings;
use crate::markdown::render_plain;
use crate::text::{display_width, truncate_to_width};
use crate::theme::TuiTheme;

/// How many collapsed lines a long block shows before folding.
pub const COLLAPSED_BODY_LINES: usize = 4;
/// The most rows a pinned prompt header may occupy.
///
/// A user prompt can be a paragraph; pinning all of it would leave no room for
/// the reply it is the header *of*. Four rows is enough to recognise the
/// question and short enough to keep the transcript the main event.
pub const MAX_STICKY_ROWS: usize = 4;
/// The blank row kept between a pinned header and the transcript below it.
pub const STICKY_GAP_ROWS: usize = 1;
/// How many rendered blocks stay resident. Blocks outside the window keep their
/// measured height but drop their styled lines.
pub const RENDER_CACHE_BLOCKS: usize = 192;
/// Upper bound on source blocks, excluding synthetic group summaries, mirroring
/// the shared controller's own timeline budget.
pub const MAX_BLOCKS: usize = 20_000;

const UNMEASURED: u32 = u32::MAX;

/// Short enough to keep a disclosure responsive on a character grid.
pub const DISCLOSURE_DURATION: Duration = Duration::from_millis(180);

struct Disclosure {
    from: f32,
    target: f32,
    progress: f32,
    started: Instant,
}

impl Disclosure {
    fn advance(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.started);
        let t = (elapsed.as_secs_f32() / DISCLOSURE_DURATION.as_secs_f32()).min(1.0);
        self.progress = self.from + (self.target - self.from) * (1.0 - (1.0 - t).powi(3));
        if t == 1.0 {
            self.progress = self.target;
        }
        t == 1.0
    }
}

/// The shortest run of collapsed work items worth folding.
pub const MIN_GROUP_RUN: usize = 3;

/// The most body rows a live thought shows before it folds.
///
/// A thought arrives a token at a time, and a block that grew with every token
/// would push the rest of the session off the screen while the reader is
/// reading it. The window is the compromise: a fixed number of rows at the
/// tail, so the newest lines push the oldest off the top instead of adding
/// height. Every row of the window is the thought's own content — the rail
/// marks the extent, and the live header marks the row as still arriving — so
/// the block is never taller than this plus its header.
pub const STREAMING_WINDOW_LINES: usize = vibex_ui::timeline::REASONING_WINDOW_LINES;

/// How far the rail's wave travels per animation tick, in radians.
///
/// The wave is `sin²`, so a full cycle is `π / RAIL_SPEED` ticks — around two
/// seconds on the animation clock: slow enough to read as flow, fast enough to
/// say the thought is still arriving.
const RAIL_SPEED: f32 = 0.42;

/// The wavelength of the rail's wave, in rows.
///
/// Longer than the tallest window, so the reader sees one crest travelling down
/// rather than a row of them pulsing at once — and a fixed length rather than a
/// fraction of the bar, so the motion looks the same as the window fills.
const RAIL_WAVE_ROWS: f32 = 8.0;

/// Whether a block has the shape of a thought that is still arriving.
///
/// The shape alone is not enough to open a window with: the runtime does not
/// close a reasoning stream, so a row that once streamed keeps that flag for the
/// rest of the turn. See [`is_live_thinking_window`].
fn is_running_thought(block: &Block) -> bool {
    block.kind == TimelineRowKind::Reasoning
        && block.streaming
        && block.expanded
        && block.reasoning_window
}

/// Whether a block opens the live window onto a thought's tail.
///
/// `trailing` says that nothing the reader can read follows the block. A
/// thought that has been left behind by a tool call or an answer is finished
/// whatever its row still claims: the window is for the thought the Agent is on
/// *now*, which is why it folds to a single row the moment the next row
/// arrives.
///
/// Such a block is *not* a one-row work item while it is live: it shows a
/// bounded window onto the tail of its body, and everything the window does —
/// the animation, the bound on its height — is gated on this
/// one predicate.
pub fn is_live_thinking_window(block: &Block, trailing: bool) -> bool {
    trailing && is_running_thought(block)
}

/// Whether a block can be folded into a dense run.
///
/// Tool details stay inside their group when opened. Expanded reasoning and
/// live thoughts stay separate so grouping cannot hide what is being read.
fn eligible_for_group(block: &Block, trailing: bool) -> bool {
    !is_live_thinking_window(block, trailing)
        && is_work_item(block.kind)
        && block.collapsible
        && (block.kind != TimelineRowKind::Reasoning || !block.expanded)
        && !block.failed
        && !block.pending_permission
        // Typed live operations stay visible. Legacy rows retain their run
        // grouping, including superseded thoughts with a stale stream flag.
        && (block.activity.is_none() || !block.streaming)
}

/// Whether `incoming` starts with older blocks than the transcript holds.
///
/// True when the transcript's current head survives somewhere after index 0,
/// which is what a prepend looks like after the diff. An empty transcript is
/// not a prepend, and neither is a reload whose head moved to the front.
fn prepends_existing_blocks(existing: &[Block], incoming: &[Block]) -> bool {
    let Some(head) = existing.iter().find(|block| !block.group.is_head()) else {
        return false;
    };
    incoming
        .iter()
        .position(|block| block.id == head.id)
        .is_some_and(|index| index > 0)
}

/// A folded turn: the process work behind the divider, and the answer below it.
///
/// A turn is a unit of work even though the transcript is a flat list of
/// blocks. Once the turn is done and the Agent has answered, the rows that
/// produced the answer fold away behind a divider that carries the turn's
/// duration — the desktop's "worked for" header, drawn between the work and the
/// answer instead of above it. The facts ride on the blocks themselves, so the
/// fold costs the layout, the search index and the copy path nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TurnFold {
    /// The projected turn this block belongs to.
    pub turn_id: String,
    /// The turn's duration, already formatted: "3m 5s".
    ///
    /// The word that frames it is product copy and stays in the locale table,
    /// so only the number travels with the projection.
    pub label: String,
    /// Whether the process section is open. The projection asks for it shut; a
    /// reader who opens it says so in the transcript's own expansion map, which
    /// is re-applied on every rebuild.
    pub expanded: bool,
    /// This block is the turn's answer, and owns the divider control.
    pub is_conclusion: bool,
    /// This block is the turn's first process row, and opens the section.
    ///
    /// An open section is bracketed: the title sits above these rows and the
    /// rule below them, so the reader meets the turn's name wherever they enter
    /// it from. Shut, there is nothing above the control to name and only the
    /// answer draws a row.
    pub is_head: bool,
    /// The string the turn's Agent brand mark is matched against.
    ///
    /// The identity rather than the runtime's label, so the mark is the one the
    /// session list draws for the same Agent: both clients ask
    /// [`vibex_brands::agent_mark`] the same question. `None` when no row of the
    /// turn named a runtime — the header then carries its words alone.
    pub agent_identity: Option<String>,
}

impl TurnFold {
    /// Whether the block is hidden because its turn's process section is shut.
    ///
    /// The answer is never hidden: it is the reason the section is folded.
    pub fn is_folded(&self) -> bool {
        !self.expanded && !self.is_conclusion
    }

    /// Whether this block draws the divider that folds the section.
    pub fn is_control(&self) -> bool {
        self.is_conclusion
    }

    /// Whether this block draws the title above an open section.
    pub fn draws_title(&self) -> bool {
        self.is_head && self.expanded
    }

    /// The identity whose mark this block's fold row draws, if any.
    ///
    /// The mark is a picture, so a terminal that cannot hold one keeps the
    /// words alone rather than the empty cells a slot would spend: the row is
    /// read for its duration either way.
    pub fn drawn_identity(&self, pictures: bool) -> Option<&str> {
        pictures
            .then_some(self.agent_identity.as_deref())
            .flatten()
            .filter(|identity| vibex_brands::agent_mark(identity).is_some())
    }
}

/// One transcript block, projected from the authoritative `TimelineRow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub id: String,
    pub kind: TimelineRowKind,
    pub title: String,
    pub body: String,
    pub turn_id: Option<String>,
    pub sequence: i64,
    /// When this block's newest row item was written, in Unix milliseconds.
    ///
    /// `None` for a block no item stands behind — one the client projected
    /// itself — and for a row the runtime never stamped. A message draws it as
    /// the clock on its right; nothing else draws it at all.
    pub timestamp_ms: Option<i64>,
    pub expanded: bool,
    /// The open body follows the live tail instead of growing with the stream.
    pub reasoning_window: bool,
    pub collapsible: bool,
    pub streaming: bool,
    pub failed: bool,
    pub pending_permission: bool,
    pub file_path: Option<String>,
    pub runtime_attribution: Option<String>,
    pub conclusion: bool,
    /// How this block participates in a dense run of work items.
    pub group: GroupRole,
    /// The turn fold this block belongs to. `None` for a block outside any
    /// settled turn — bookkeeping the runtime wrote between turns, or work the
    /// Agent is still doing.
    pub fold: Option<TurnFold>,
    /// Desktop-derived activity identity, separate from its expandable output.
    pub activity: Option<Activity>,
    /// Raw bounded fields; decoding happens only when this block is rendered.
    pub details: Vec<Detail>,
    /// Derived from all members of an activity group.
    pub group_summary: Option<ActivitySummary>,
}

impl Block {
    /// Content identity used for dirty checking: two blocks with the same key
    /// render identically, so a streaming append changes it every time.
    fn content_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        kind_id(self.kind).hash(&mut hasher);
        self.title.hash(&mut hasher);
        self.body.hash(&mut hasher);
        self.expanded.hash(&mut hasher);
        self.reasoning_window.hash(&mut hasher);
        self.streaming.hash(&mut hasher);
        self.failed.hash(&mut hasher);
        self.pending_permission.hash(&mut hasher);
        self.collapsible.hash(&mut hasher);
        self.file_path.hash(&mut hasher);
        self.group.hash(&mut hasher);
        self.fold.hash(&mut hasher);
        self.activity.hash(&mut hasher);
        self.details.hash(&mut hasher);
        self.group_summary.hash(&mut hasher);
        self.runtime_attribution.hash(&mut hasher);
        // Not content, but drawn from the block: a row whose clock moved has to
        // be laid out again, or the transcript would keep showing the hour the
        // row was first measured at.
        self.timestamp_ms.hash(&mut hasher);
        hasher.finish()
    }

    /// Whether the block shows its whole body.
    pub fn is_open(&self) -> bool {
        self.expanded || !self.collapsible
    }

    /// Whether the block draws no rows at all.
    ///
    /// Two things hide a block: being a folded member of a dense run, and
    /// being folded away with the rest of its turn's process section. Both are
    /// separate from the block's own expansion, which only shortens its body.
    pub fn is_hidden(&self) -> bool {
        self.group.is_hidden() || self.fold.as_ref().is_some_and(TurnFold::is_folded)
    }
}

/// A rendered block: its display lines and the plain text of each.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderedBlock {
    pub lines: Vec<Line<'static>>,
    pub plain: Vec<String>,
    pub height: usize,
    /// The picture the block wants the terminal to hold over one of its rows.
    pub mark: Option<RowMark>,
}

/// A picture a transcript row wants drawn over the cells it reserved.
///
/// The rows of a transcript are text to ratatui, and a brand mark is not text:
/// the row keeps a slot of blank cells and this says what to put in it, in the
/// coordinates of the block. The renderer turns it into a screen rectangle once
/// the window is known, exactly as the session list does with its own marks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowMark {
    /// The row of the block the picture sits on, counting from its first.
    pub line: usize,
    /// The picture's first column, from the left edge of the transcript band.
    pub column: u16,
    /// The identity string [`vibex_brands::agent_mark`] is asked for.
    pub identity: String,
}

/// One mark a rendered window wants, in the window's own coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowMark {
    /// The row of the window the picture sits on, counting from its first.
    pub row: usize,
    /// The picture's first column, from the left edge of the band.
    pub column: u16,
    /// The identity string [`vibex_brands::agent_mark`] is asked for.
    pub identity: String,
}

impl RenderedBlock {
    pub fn text(&self) -> String {
        self.plain.join("\n")
    }
}

/// One display line read back as text.
///
/// [`Transcript::plain_lines`] answers in the same cells the mouse speaks in:
/// the band's columns, whose first few are chrome on every row rather than
/// text. `chrome` is how many, so a column under the pointer can be read as a
/// column of `text` — and never as a column of the marker beside it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlainLine {
    pub text: String,
    pub chrome: usize,
}

/// What changed during the last [`Transcript::set_blocks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChangeSet {
    pub replaced: usize,
    pub appended: usize,
    pub removed: usize,
    pub any: bool,
}

/// The viewport scroll state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollState {
    /// Offset in display lines from the top of the transcript.
    pub offset: usize,
    /// Whether the view sticks to the bottom as new content arrives.
    pub follow: bool,
    /// Block drawn as the current one, if any.
    pub selected: Option<usize>,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            offset: 0,
            follow: true,
            selected: None,
        }
    }
}

/// A user prompt pinned above the transcript viewport.
#[derive(Debug, Clone)]
pub struct StickyHeader {
    /// The block the header was drawn from, so a click can jump to it.
    pub block: usize,
    pub lines: Vec<Line<'static>>,
}

impl StickyHeader {
    /// Rows the header occupies, gap included.
    pub fn reserved_rows(&self) -> usize {
        self.lines.len() + STICKY_GAP_ROWS
    }
}

/// The transcript model.
pub struct Transcript {
    blocks: Vec<Block>,
    reasoning_expanded_by_default: bool,
    reasoning_expansion_mode: ReasoningExpansionMode,
    reasoning_expansion: HashMap<String, bool>,
    /// The reader's explicit choice to open or fold a turn's process section.
    ///
    /// Keyed by the projected turn, not by a block: the process rows are many
    /// and the header that stands for them is synthetic, so the choice has to
    /// outlive both. A turn with no entry takes the projection's default, which
    /// is what makes a turn fold itself the moment it settles.
    turn_expansion: HashMap<String, bool>,
    motion_enabled: bool,
    disclosures: HashMap<String, Disclosure>,
    /// Derived indices, kept outside the content key so a prepend can reuse rows.
    parents: Vec<Option<usize>>,
    /// Per-block measured height, or [`UNMEASURED`].
    heights: Vec<u32>,
    /// Per-block content key at the time the height was measured.
    keys: Vec<u64>,
    /// The incremental markdown renderer of each block that is still arriving.
    ///
    /// A streaming block is re-rendered on every delta; keeping the frozen
    /// prefix here is what makes that cost the size of the unfrozen tail rather
    /// than the size of the answer.
    live: std::collections::HashMap<String, crate::markdown::StreamingMarkdown>,
    /// Prefix sums over `heights`, using the estimate for unmeasured slots.
    offsets: Vec<usize>,
    layout_valid: bool,
    rendered: HashMap<usize, RenderedBlock>,
    /// Recency order for the render cache, oldest first.
    recency: Vec<usize>,
    width: usize,
    theme_id: String,
    /// The chrome animation clock as of the last frame.
    ///
    /// Only a live thought's rail moves with it, so the clock is kept here
    /// rather than in the cache key: a phase change drops the window's rows and
    /// leaves every other block's rendering alone.
    phase: u32,
    /// The block drawn as current last frame, so a change invalidates it.
    last_selected: Option<usize>,
    /// Display line the viewport started at in the last frame.
    scroll_offset: usize,
    /// Rows the viewport held in the last frame.
    ///
    /// Kept beside [`Transcript::scroll_offset`], and for the same reason: the
    /// rail asks which turns are on screen, and the answer is the turns between
    /// the line the viewport starts at and the last line it holds.
    viewport_rows: usize,
    /// Counters that make the cache behaviour observable in tests and in the
    /// benchmark harness.
    pub stats: TranscriptStats,
    /// Whether the terminal can hold a picture, so a fold's title reserves the
    /// cells its Agent's mark is drawn over.
    ///
    /// A terminal that cannot draw one keeps the words alone: the slot would be
    /// air, and a row that indents its title for nothing reads as a mistake.
    pictures: bool,
    /// The marks the last window asked for, in the window's own coordinates.
    ///
    /// Published by [`Transcript::visible_lines`] for the renderer that owns
    /// the terminal: a picture has no clip rect, so it may only be placed by
    /// the layer that knows which rows the window actually drew.
    window_marks: Vec<WindowMark>,
}

/// Observable counters. The performance contract is stated in terms of these.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TranscriptStats {
    /// Blocks whose styled lines were built because the viewport needed them.
    pub blocks_rendered: u64,
    /// Blocks whose height was measured.
    pub blocks_measured: u64,
    /// Height measurements served from cache.
    pub measure_hits: u64,
    /// Frames that produced no work because nothing changed.
    pub idle_frames: u64,
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            reasoning_expanded_by_default: false,
            reasoning_expansion_mode: ReasoningExpansionMode::Window,
            reasoning_expansion: HashMap::new(),
            turn_expansion: HashMap::new(),
            motion_enabled: false,
            disclosures: HashMap::new(),
            parents: Vec::new(),
            heights: Vec::new(),
            keys: Vec::new(),
            live: std::collections::HashMap::new(),
            offsets: Vec::new(),
            layout_valid: false,
            rendered: HashMap::new(),
            recency: Vec::new(),
            width: 0,
            theme_id: String::new(),
            phase: 0,
            last_selected: None,
            scroll_offset: 0,
            viewport_rows: 0,
            stats: TranscriptStats::default(),
            pictures: false,
            window_marks: Vec::new(),
        }
    }

    /// Tell the transcript whether the terminal can hold a picture.
    ///
    /// The capability is the renderer's to know — it owns the terminal — and
    /// the layout's to obey, because the cells a mark is drawn over are
    /// reserved by the row that draws it. A change drops every measured height:
    /// whether a fold's title leads with a mark is part of its shape.
    pub fn set_pictures(&mut self, pictures: bool) {
        if self.pictures == pictures {
            return;
        }
        self.pictures = pictures;
        for index in 0..self.blocks.len() {
            self.invalidate(index);
        }
    }

    /// The pictures the last rendered window wants drawn over it.
    pub fn window_marks(&self) -> &[WindowMark] {
        &self.window_marks
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The newest timeline sequence the transcript holds.
    ///
    /// A send is confirmed by a row *newer* than everything the reader could
    /// already see, which is what this measures.
    pub fn newest_sequence(&self) -> i64 {
        self.blocks
            .iter()
            .map(|block| block.sequence)
            .max()
            .unwrap_or(0)
    }

    pub fn block(&self, index: usize) -> Option<&Block> {
        self.blocks.get(index)
    }

    /// Index of the block with this id, if it is still in the transcript.
    ///
    /// Used to keep the reader's place across a prepend: the block is
    /// remembered before the reload and its new first line is looked up after.
    pub fn index_of_block(&self, id: &str) -> Option<usize> {
        self.blocks.iter().position(|block| block.id == id)
    }

    pub fn visible_block(&self, index: usize) -> Option<usize> {
        let block = self.blocks.get(index)?;
        // A block folded away with its turn's process section has no parent
        // block to point at: the row that stands for the hidden section is the
        // conclusion, which is where the fold control lives.
        if block.fold.as_ref().is_some_and(TurnFold::is_folded) {
            let turn_id = block.fold.as_ref().map(|fold| fold.turn_id.as_str())?;
            return self.blocks.iter().position(|candidate| {
                candidate
                    .fold
                    .as_ref()
                    .is_some_and(|fold| fold.turn_id == turn_id && fold.is_control())
            });
        }
        if block.group.is_hidden() {
            self.parents[index]
        } else {
            Some(index)
        }
    }

    /// Search reveals the matching item inside its group, not the next row at
    /// the same collapsed offset. Jumping to it settles that disclosure first.
    pub fn reveal_block(&mut self, index: usize) {
        if index >= self.blocks.len() {
            return;
        }
        // A match inside a folded process section has to open the fold first,
        // or the transcript would scroll to a row that is not drawn and land on
        // the answer instead. The fold control is on the conclusion, which is
        // the visible row that stands for the hidden section.
        if self.blocks[index]
            .fold
            .as_ref()
            .is_some_and(TurnFold::is_folded)
        {
            let control = self.visible_block(index);
            if let Some(control) = control {
                self.toggle_turn_fold(control);
            }
        }
        if let Some(parent) = self.parents[index] {
            if !self.blocks[parent].expanded {
                self.toggle_block(parent);
            }
            if self.disclosures.remove(&self.blocks[parent].id).is_some() {
                self.invalidate_disclosure(parent);
            }
        }
        if self.blocks[index].collapsible && !self.blocks[index].expanded {
            self.set_block_expanded(index, true);
        }
        if self.disclosures.remove(&self.blocks[index].id).is_some() {
            self.invalidate_disclosure(index);
        }
    }

    /// Total display height, measured plus estimated.
    pub fn total_height(&mut self) -> usize {
        self.ensure_layout();
        self.offsets.last().copied().unwrap_or(0)
    }

    /// Replace the transcript contents with `blocks`.
    ///
    /// Blocks are matched by id, so an append, a single streaming update, and a
    /// wholesale reload all collapse into the same diff.
    pub fn set_blocks(&mut self, blocks: Vec<Block>) -> ChangeSet {
        let mut change = ChangeSet::default();
        // Headers are presentation nodes, never input to a second grouping pass.
        let blocks = blocks
            .into_iter()
            .filter(|block| !block.group.is_head())
            .collect::<Vec<_>>();
        // Trim when the authority's budget is exceeded. A prepend is the one
        // case where the incoming list starts with history the reader just
        // asked for, so dropping from the front would delete exactly what the
        // fetch was for; an append or a wholesale reload keeps the newest
        // blocks instead, which is the behaviour that predates the cursor.
        let mut blocks = if blocks.len() > MAX_BLOCKS {
            change.removed += blocks.len() - MAX_BLOCKS;
            if prepends_existing_blocks(&self.blocks, &blocks) {
                let mut trimmed = blocks;
                trimmed.truncate(MAX_BLOCKS);
                trimmed
            } else {
                blocks[blocks.len() - MAX_BLOCKS..].to_vec()
            }
        } else {
            blocks
        };

        let existing: HashMap<&str, usize> = self
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.id.as_str(), index))
            .collect();

        for block in &mut blocks {
            if let Some(&index) = existing.get(block.id.as_str()) {
                block.expanded = self.blocks[index].expanded;
            }
            block.group = GroupRole::Solo;
            block.group_summary = None;
        }

        // A reader's explicit choice about a turn's process section outlives a
        // rebuild exactly as a reasoning expansion does; the projection supplies
        // the default the choice overrides. Choices leave with the turn that
        // owns them, so the map cannot grow with the session.
        if !self.turn_expansion.is_empty() {
            let turns = blocks
                .iter()
                .filter_map(|block| block.fold.as_ref().map(|fold| fold.turn_id.as_str()))
                .collect::<HashSet<_>>();
            self.turn_expansion
                .retain(|turn_id, _| turns.contains(turn_id.as_str()));
        }
        for block in &mut blocks {
            if let Some(fold) = block.fold.as_mut()
                && let Some(&expanded) = self.turn_expansion.get(&fold.turn_id)
            {
                fold.expanded = expanded;
            }
        }
        // Explicit choices outlive a temporarily absent bottom indicator, but
        // leave with the turn/session that owns them.
        if !self.reasoning_expansion.is_empty() {
            let retained = blocks
                .iter()
                .map(|block| block.id.as_str())
                .collect::<HashSet<_>>();
            let turns = blocks
                .iter()
                .filter_map(|block| block.turn_id.as_deref())
                .collect::<HashSet<_>>();
            self.reasoning_expansion.retain(|id, _| {
                retained.contains(id.as_str())
                    || id
                        .strip_prefix("reasoning-live:")
                        .is_some_and(|turn| turns.contains(turn))
            });
        }
        let last = blocks.len().checked_sub(1);
        for (index, block) in blocks.iter_mut().enumerate() {
            self.resolve_reasoning(block, Some(index) == last);
        }
        let blocks = self.group_blocks(blocks, &existing);

        let mut next_blocks = Vec::with_capacity(blocks.len());
        let mut next_heights = Vec::with_capacity(blocks.len());
        let mut next_keys = Vec::with_capacity(blocks.len());
        let mut reused_rendered: HashMap<usize, RenderedBlock> = HashMap::new();
        let mut reused_recency = Vec::new();

        for (new_index, block) in blocks.into_iter().enumerate() {
            let key = block.content_key();
            match existing.get(block.id.as_str()) {
                Some(&old_index) if self.keys.get(old_index) == Some(&key) => {
                    // Unchanged: keep the measured height and, if resident, the
                    // rendered lines.
                    next_heights.push(self.heights[old_index]);
                    next_keys.push(key);
                    if let Some(rendered) = self.rendered.remove(&old_index) {
                        reused_rendered.insert(new_index, rendered);
                        reused_recency.push(new_index);
                    }
                }
                Some(_) => {
                    change.replaced += 1;
                    next_heights.push(UNMEASURED);
                    next_keys.push(key);
                }
                None => {
                    change.appended += 1;
                    next_heights.push(UNMEASURED);
                    next_keys.push(key);
                }
            }
            next_blocks.push(block);
        }

        change.removed += self
            .blocks
            .len()
            .saturating_sub(next_blocks.len() + change.removed);
        change.any = change.replaced > 0 || change.appended > 0 || change.removed > 0;

        // A live renderer belongs to a block that is still in the transcript;
        // one whose block has gone would otherwise sit in the map forever.
        let present = next_blocks
            .iter()
            .map(|block| block.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        self.live.retain(|id, _| present.contains(id.as_str()));
        self.disclosures
            .retain(|id, _| present.contains(id.as_str()));

        // A dense row's rendering depends on its neighbours as well as on
        // itself: the gap it leaves is decided by whether the row after it is
        // also a work item, and whether it carries the runtime's name by the row
        // before. A block that changed therefore makes its *predecessor* stale —
        // but only when either of them is a work item, which is the case the
        // neighbours decide.
        for index in 1..next_blocks.len() {
            let changed = next_heights[index] == UNMEASURED;
            let neighbouring =
                is_dense_row(next_blocks[index].kind) || is_dense_row(next_blocks[index - 1].kind);
            if !changed || !neighbouring || next_heights[index - 1] == UNMEASURED {
                continue;
            }
            next_heights[index - 1] = UNMEASURED;
            reused_rendered.remove(&(index - 1));
            reused_recency.retain(|value| *value != index - 1);
        }

        self.blocks = next_blocks;
        let mut parent = None;
        self.parents = self
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| match block.group {
                GroupRole::Head { .. } => {
                    parent = Some(index);
                    None
                }
                GroupRole::Member { .. } => parent,
                GroupRole::Solo => {
                    parent = None;
                    None
                }
            })
            .collect();
        self.heights = next_heights;
        self.keys = next_keys;
        self.rendered = reused_rendered;
        self.recency = reused_recency;
        self.layout_valid = false;
        change
    }

    /// A group has its own stable header, so opening a tool never removes the
    /// control that closes the group. Detail expansion does not split a run.
    fn group_blocks(&self, blocks: Vec<Block>, existing: &HashMap<&str, usize>) -> Vec<Block> {
        let mut grouped = Vec::with_capacity(blocks.len());
        let mut blocks = blocks.into_iter().peekable();
        while let Some(first) = blocks.next() {
            if !eligible_for_group(&first, blocks.len() == 0) {
                grouped.push(first);
                continue;
            }
            let semantic = first.activity.is_some();
            let mut run = vec![first];
            loop {
                let trailing = blocks.len() == 1;
                let Some(next) = blocks.peek() else { break };
                if !eligible_for_group(next, trailing)
                    || next.turn_id != run[0].turn_id
                    || next.runtime_attribution != run[0].runtime_attribution
                    || if semantic {
                        next.activity.is_none()
                    } else {
                        next.kind != run[0].kind || next.title != run[0].title
                    }
                {
                    break;
                }
                run.extend(blocks.next());
            }
            let members = run.len();
            if members < MIN_GROUP_RUN {
                grouped.extend(run);
                continue;
            }
            let first = &run[0];
            let id = format!("activity-group:{}", first.id);
            let expanded = existing
                .get(id.as_str())
                .map(|&old| self.blocks[old].expanded)
                .unwrap_or_else(|| run.iter().any(|block| block.expanded));
            let mut head = Block {
                id,
                kind: first.kind,
                title: first.title.clone(),
                body: String::new(),
                turn_id: first.turn_id.clone(),
                sequence: first.sequence,
                timestamp_ms: None,
                expanded,
                reasoning_window: false,
                collapsible: true,
                streaming: false,
                failed: false,
                pending_permission: false,
                file_path: None,
                runtime_attribution: None,
                conclusion: false,
                group: GroupRole::Head { members },
                // The head stands for the run, so it carries the run's turn
                // fold: a head left unfolded would stay on screen after the
                // rest of its turn's process folded away.
                fold: first.fold.clone(),
                activity: None,
                details: Vec::new(),
                group_summary: None,
            };
            if semantic {
                let mut summary = ActivitySummary::default();
                for block in &run {
                    if let Some(activity) = &block.activity {
                        summary.record(activity, &block.id);
                    }
                }
                head.group_summary = Some(summary);
            }
            let visible = head.expanded;
            grouped.push(head);
            for (offset, mut member) in run.into_iter().enumerate() {
                // The head took the run's fold, and with it the title above the
                // section. The row the head was minted from is a member now, and
                // a member left carrying `is_head` draws the turn's name a
                // second time — directly under the head that already says it.
                if let Some(fold) = member.fold.as_mut() {
                    fold.is_head = false;
                }
                member.group = GroupRole::Member {
                    visible,
                    last: offset + 1 == members,
                };
                grouped.push(member);
            }
        }
        grouped
    }

    /// The block that opens each turn, in conversation order.
    ///
    /// A turn is one the reader opened. The projection attributes runs of
    /// blocks to a turn, and a run counts when the message that began the
    /// conversation is in it: the runtime also writes rows of its own — the
    /// lines a session appends as it starts up, the hidden continuation that
    /// follows a failed turn — and a run with no message in it is that work,
    /// not a turn the reader took. Counting those put a tick on the rail for a
    /// conversation that had not begun, and one more for every restart.
    ///
    /// The rail needs the head rather than the turn's key: the send the runtime
    /// has not echoed yet carries no key at all, and it is a turn of the
    /// conversation from the moment it is drawn.
    fn turn_heads(&self) -> Vec<usize> {
        let mut heads: Vec<usize> = Vec::new();
        let mut head: Option<usize> = None;
        let mut key: Option<&str> = None;
        for (index, block) in self.blocks.iter().enumerate() {
            let block_key = block.turn_id.as_deref();
            if index > 0 && block_key != key {
                heads.extend(head.take());
            }
            key = block_key;
            if head.is_none() && block.kind == TimelineRowKind::UserMessage {
                head = Some(index);
            }
        }
        heads.extend(head);
        heads
    }

    /// How many turns the transcript contains.
    pub fn turn_count(&self) -> usize {
        self.turn_heads().len()
    }

    /// The turn the viewport top is on, for the rail's current-tick marker.
    ///
    /// A row that belongs to no turn of its own reads as the turn above it: a
    /// notice between turns is the tail of the turn it follows, and a
    /// continuation is more of the turn that failed before it. Above the first
    /// turn there is nothing to mark, and no tick wears the marker.
    pub fn active_turn(&mut self) -> Option<usize> {
        self.ensure_layout();
        let offset = self.scroll_offset;
        let index = self.block_at_line(offset)?;
        self.turn_heads()
            .partition_point(|head| *head <= index)
            .checked_sub(1)
    }

    /// The turns the viewport shows, for the rail's lit window.
    ///
    /// The range starts at [`Transcript::active_turn`] — the turn the viewport
    /// starts on, which the rail marks with its heaviest tick — and ends at the
    /// turn the last visible row belongs to. Every turn between the two ends is
    /// on screen as well, because a turn is a contiguous run of blocks: the
    /// rail lights a window of the conversation, not a single point in it.
    ///
    /// A window that opens above the first turn lights from the first turn
    /// down: the runtime's own startup rows belong to no turn, but a screen that
    /// reaches the conversation is showing it. A viewport with no rows, or one
    /// that stops above every turn, shows nothing.
    pub fn visible_turns(&mut self) -> Option<Range<usize>> {
        if self.viewport_rows == 0 {
            return None;
        }
        self.ensure_layout();
        let bottom_line = self.scroll_offset.saturating_add(self.viewport_rows - 1);
        let bottom = self.block_at_line(bottom_line)?;
        let last = self
            .turn_heads()
            .partition_point(|head| *head <= bottom)
            .checked_sub(1)?;
        let first = self.active_turn().unwrap_or(0);
        Some(first..last + 1)
    }

    /// The block that opens the `turn`-th turn, for a click on the rail.
    pub fn block_of_turn(&self, turn: usize) -> Option<usize> {
        self.turn_heads().get(turn).copied()
    }

    /// The line offset the viewport currently starts at.
    ///
    /// Kept in sync by [`Transcript::visible_lines`] so the rail and the
    /// scrollbar can be computed without re-deriving it from the scroll state.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// The lowest offset a viewport of `height` rows can hold.
    ///
    /// The bottom of the transcript is the last row of content at the last row
    /// of the viewport, not the last row at the top of it: a reader who scrolls
    /// past the bottom would be scrolling into blank space, and would keep
    /// scrolling, because an offset has no ceiling of its own. The tail is
    /// measured first, so the answer is the real height rather than an estimate
    /// of blocks that have never been drawn.
    pub fn bottom_offset(&mut self, height: usize, theme: &TuiTheme, strings: Strings) -> usize {
        if height == 0 {
            return 0;
        }
        self.measure_tail(height, theme, strings);
        self.total_height().saturating_sub(height)
    }

    /// Whether any block is currently working.
    pub fn is_animating(&self) -> bool {
        !self.disclosures.is_empty() || self.blocks.iter().any(|block| block.streaming)
    }

    pub fn set_reasoning_preferences(&mut self, expanded: bool, mode: ReasoningExpansionMode) {
        self.reasoning_expanded_by_default = expanded;
        self.reasoning_expansion_mode = mode;
    }

    /// Settings that change the default replace the previous per-row choices.
    pub fn clear_reasoning_expansion(&mut self) {
        self.reasoning_expansion.clear();
    }

    pub fn set_motion_enabled(&mut self, enabled: bool) {
        self.motion_enabled = enabled;
        if !enabled && !self.disclosures.is_empty() {
            let ids = self
                .disclosures
                .drain()
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            for id in ids {
                if let Some(index) = self.index_of_block(&id) {
                    self.invalidate_disclosure(index);
                }
            }
        }
    }

    fn begin_disclosure(&mut self, index: usize, expanded: bool) {
        if !self.motion_enabled || !self.rendered.contains_key(&index) {
            return;
        }
        let block = &self.blocks[index];
        let from = self
            .disclosures
            .get(&block.id)
            .map_or(if block.expanded { 1.0 } else { 0.0 }, |disclosure| {
                disclosure.progress
            });
        let target = if expanded { 1.0 } else { 0.0 };
        if from == target {
            self.disclosures.remove(&block.id);
            return;
        }
        self.disclosures.insert(
            block.id.clone(),
            Disclosure {
                from,
                target,
                progress: from,
                started: Instant::now(),
            },
        );
    }

    /// Advance only active disclosures. The last tick still draws the resting
    /// frame, after which an idle transcript schedules no further repaint.
    pub fn advance_disclosures(&mut self, now: Instant) -> bool {
        if self.disclosures.is_empty() {
            return false;
        }
        let mut finished = Vec::new();
        for (id, disclosure) in &mut self.disclosures {
            if disclosure.advance(now) {
                finished.push(id.clone());
            }
        }
        for id in finished {
            self.disclosures.remove(&id);
            if let Some(index) = self.index_of_block(&id) {
                self.invalidate_disclosure(index);
            }
        }
        self.layout_valid = false;
        true
    }

    fn invalidate_disclosure(&mut self, index: usize) {
        self.invalidate(index);
        if let GroupRole::Head { members } = self.blocks[index].group {
            for member in index + 1..(index + members + 1).min(self.blocks.len()) {
                self.invalidate(member);
            }
        }
    }

    fn disclosure_progress(&self, index: usize) -> f32 {
        let block = &self.blocks[index];
        self.disclosures
            .get(&block.id)
            .map_or(if block.expanded { 1.0 } else { 0.0 }, |disclosure| {
                disclosure.progress
            })
    }

    fn body_is_drawn(&self, index: usize) -> bool {
        self.blocks[index].expanded || self.disclosure_progress(index) > 0.0
    }

    fn block_is_drawn(&self, index: usize) -> bool {
        // A block folded away with its turn's process section draws nothing,
        // however far any group disclosure has animated: the fold is a state of
        // the turn, not a partially open run.
        if self.blocks[index]
            .fold
            .as_ref()
            .is_some_and(TurnFold::is_folded)
        {
            return false;
        }
        !self.blocks[index].group.is_hidden()
            || self.parents[index].is_some_and(|parent| self.disclosure_progress(parent) > 0.0)
    }

    fn resolve_reasoning(&self, block: &mut Block, trailing: bool) {
        if block.kind != TimelineRowKind::Reasoning || block.group.is_head() {
            return;
        }
        let live = trailing && block.streaming;
        // The window is the shape of an open body, not an invitation: a live
        // thought is windowed only while the mode asks for one, but it opens
        // only when the reader chose to open it or default expansion is on.
        let window = live && self.reasoning_expansion_mode == ReasoningExpansionMode::Window;
        block.expanded = self
            .reasoning_expansion
            .get(&block.id)
            .copied()
            .unwrap_or(self.reasoning_expanded_by_default);
        block.reasoning_window = window;
    }

    /// Tell the transcript which width and theme it will render at.
    ///
    /// A change invalidates every measurement, because wrapping is width
    /// dependent. Callers normally pass the same values every frame, in which
    /// case this is free.
    pub fn configure(&mut self, width: usize, theme: &TuiTheme) {
        let width = width.max(8);
        if self.width != width {
            self.width = width;
            self.invalidate_all();
        }
        self.set_theme(theme);
    }

    /// Tell the transcript how far the chrome animation clock has moved.
    ///
    /// The live thought window is the one block whose *rows* depend on the
    /// clock: its rail is a frame of the running animation. A phase change
    /// therefore drops those rows and no measurements — the window is the same
    /// height, only lit differently — so the next frame redraws the wave
    /// without a relayout.
    pub fn set_animation_phase(&mut self, phase: u32) {
        if self.phase == phase {
            return;
        }
        self.phase = phase;
        // There is at most one live window — it is the last row there is — so
        // the clock's cost is a lookup, not a walk over the session.
        if let Some(last) = self.last_visible_block()
            && is_live_thinking_window(&self.blocks[last], true)
        {
            self.rendered.remove(&last);
            self.recency.retain(|value| *value != last);
        }
    }

    /// The last block that draws any rows.
    ///
    /// A folded member is not one: the run's head stands for it, so the head is
    /// what "the end of the session" means.
    fn last_visible_block(&self) -> Option<usize> {
        self.blocks.iter().rposition(|block| !block.is_hidden())
    }

    /// Drop every measurement and rendered row.
    fn invalidate_all(&mut self) {
        self.heights
            .iter_mut()
            .for_each(|height| *height = UNMEASURED);
        self.rendered.clear();
        self.recency.clear();
        self.live.clear();
        self.layout_valid = false;
    }

    /// Invalidate the cache when the look changed.
    ///
    /// The width is the renderer's business — it is the only code that knows
    /// how wide the transcript band actually is — so a look change must be
    /// tellable without pretending to know a width.
    pub fn set_theme(&mut self, theme: &TuiTheme) {
        let theme_id = theme.id.to_string();
        if self.theme_id == theme_id {
            return;
        }
        self.theme_id = theme_id;
        self.invalidate_all();
    }

    /// Toggle one block's expansion.
    ///
    /// A conclusion that draws a fold divider and has no body of its own hands
    /// `e` to the divider: the divider is the row the reader sees first, and
    /// the work it hides is what they came to open. A conclusion that is itself
    /// collapsible — an error whose message is worth reading in full — keeps
    /// `e` for its body, because its header advertises the expansion the key's
    /// own help names; the divider below is still a click target. Either way
    /// `e` runs the affordance the block's own rows advertise, so the key and
    /// the hint beside it cannot disagree.
    pub fn toggle_block(&mut self, index: usize) -> bool {
        let fold_control = self
            .blocks
            .get(index)
            .and_then(|block| block.fold.as_ref())
            .is_some_and(TurnFold::is_control);
        let own_body = self
            .blocks
            .get(index)
            .is_some_and(|block| block.collapsible);
        if fold_control && !own_body {
            return self.toggle_turn_fold(index);
        }
        let Some(block) = self.blocks.get(index) else {
            return false;
        };
        if !block.collapsible || block.is_hidden() {
            return false;
        }
        let expanded = !block.expanded;
        self.set_block_expanded(index, expanded);
        if let GroupRole::Head { members } = self.blocks[index].group {
            for member in index + 1..(index + members + 1).min(self.blocks.len()) {
                // Opening reveals headers only. Closing also resets every
                // detail, so reopening a group never dumps its old payloads.
                if !expanded {
                    self.set_block_expanded(member, false);
                }
                if let GroupRole::Member { visible, .. } = &mut self.blocks[member].group {
                    *visible = expanded;
                }
                self.keys[member] = self.blocks[member].content_key();
                self.invalidate(member);
            }
        }
        true
    }

    /// Flip the process section of the turn whose divider is at `index`.
    ///
    /// The pointer resolves the row it landed on rather than re-deriving which
    /// affordance `e` would have chosen, so a click on the divider always means
    /// the divider.
    pub fn toggle_fold_at(&mut self, index: usize) -> bool {
        self.toggle_turn_fold(index)
    }

    /// Flip the process section of the turn whose fold control is at `index`.
    ///
    /// The reader's choice is recorded against the turn, not the block, so it
    /// survives the rebuild that follows the next streaming delta — and it is
    /// applied to every block of the turn in one pass, which is what makes the
    /// fold a property of the turn rather than a coincidence of neighbouring
    /// blocks agreeing.
    fn toggle_turn_fold(&mut self, index: usize) -> bool {
        let Some(expanded) = self
            .blocks
            .get(index)
            .and_then(|block| block.fold.as_ref())
            .map(|fold| !fold.expanded)
        else {
            return false;
        };
        self.set_turn_fold(index, expanded)
    }

    /// Set the process section of the turn whose fold control is at `index`.
    ///
    /// Either end of the section answers: the title above it and the rule below
    /// it are the same gesture, so they resolve to the same turn here. The
    /// answer is where the projection put the control, which is what the fold's
    /// own state is read from when the projection is rebuilt.
    fn set_turn_fold(&mut self, index: usize, expanded: bool) -> bool {
        let Some(turn_id) = self
            .blocks
            .get(index)
            .and_then(|block| block.fold.as_ref())
            .map(|fold| fold.turn_id.clone())
        else {
            return false;
        };
        self.turn_expansion.insert(turn_id.clone(), expanded);
        for index in 0..self.blocks.len() {
            let Some(fold) = self.blocks[index].fold.as_ref() else {
                continue;
            };
            if fold.turn_id != turn_id {
                continue;
            }
            // Whether the fold decides this row is on screen at all: a row the
            // shut section hides was measured at nothing, and the room it takes
            // when the section opens is not the room it was measured in.
            let drawn = self.block_is_drawn(index);
            if let Some(fold) = self.blocks[index].fold.as_mut() {
                fold.expanded = expanded;
            }
            self.keys[index] = self.blocks[index].content_key();
            // A row the fold neither shows nor hides keeps what it measured, and
            // only its lines are stale: the title and the rule are drawn on rows
            // the turn was already spending, and the answer spends one row and
            // one row of air whichever of the two it carries. Keeping that
            // height is what lets a reader parked in the section keep their
            // place — putting the answer back on its estimate would move them by
            // the difference, and the estimate is nowhere near the rows markdown
            // draws.
            if self.block_is_drawn(index) == drawn {
                self.drop_lines(index);
            } else {
                self.invalidate(index);
            }
        }
        self.layout_valid = false;
        true
    }

    fn set_block_expanded(&mut self, index: usize, expanded: bool) {
        if self.blocks[index].expanded != expanded {
            self.begin_disclosure(index, expanded);
        }
        let block = &mut self.blocks[index];
        block.expanded = expanded;
        if block.kind == TimelineRowKind::Reasoning && !block.group.is_head() {
            self.reasoning_expansion.insert(block.id.clone(), expanded);
            block.reasoning_window =
                block.streaming && self.reasoning_expansion_mode == ReasoningExpansionMode::Window;
        }
        self.keys[index] = self.blocks[index].content_key();
        self.invalidate(index);
        if index > 0 {
            self.invalidate(index - 1);
        }
    }

    /// Expand or collapse every collapsible block at once.
    ///
    /// A turn's process section folds for the reader the same way a block's
    /// body does, so "expand all" opens it too: one key that reveals everything
    /// is the point of the key, and a reader who pressed it to find a hidden
    /// tool row would otherwise have to hunt for each turn's divider by hand.
    pub fn toggle_all(&mut self, expanded: bool) {
        for index in 0..self.blocks.len() {
            if self.blocks[index]
                .fold
                .as_ref()
                .is_some_and(TurnFold::is_control)
            {
                self.set_turn_fold(index, expanded);
                continue;
            }
            if !self.blocks[index].collapsible {
                continue;
            }
            self.set_block_expanded(index, expanded);
            if let GroupRole::Member { visible, .. } = &mut self.blocks[index].group {
                *visible = expanded;
            }
            self.keys[index] = self.blocks[index].content_key();
        }
    }

    pub fn toggle_reasoning(&mut self) {
        let expanded = self
            .blocks
            .iter()
            .any(|block| block.kind == TimelineRowKind::Reasoning && !block.expanded);
        for index in 0..self.blocks.len() {
            if self.blocks[index].kind != TimelineRowKind::Reasoning {
                continue;
            }
            self.set_block_expanded(index, expanded);
            if let GroupRole::Member { visible, .. } = &mut self.blocks[index].group {
                *visible = expanded;
            }
            self.keys[index] = self.blocks[index].content_key();
        }
    }

    /// Whether every collapsible block is currently open.
    ///
    /// A shut turn fold counts as closed: "expand all" is offered while
    /// anything on screen is still hidden, and pressing it must open the fold
    /// as well as the bodies inside it.
    pub fn all_expanded(&self) -> bool {
        self.blocks
            .iter()
            .filter(|block| {
                block.collapsible || block.fold.as_ref().is_some_and(TurnFold::is_control)
            })
            .all(|block| {
                if block.fold.as_ref().is_some_and(TurnFold::is_control) {
                    block.fold.as_ref().is_some_and(|fold| fold.expanded)
                } else {
                    block.expanded
                }
            })
    }

    fn invalidate(&mut self, index: usize) {
        if let Some(height) = self.heights.get_mut(index) {
            *height = UNMEASURED;
        }
        self.drop_lines(index);
        self.layout_valid = false;
    }

    /// Drop a block's cached lines while keeping the height it measured at.
    ///
    /// A row can be redrawn without changing the room it takes — the fold's own
    /// row swaps a title for a rule on the same line — and re-measuring it then
    /// would throw a parked reader onto an estimate for a shape that never
    /// changed.
    fn drop_lines(&mut self, index: usize) {
        self.rendered.remove(&index);
        self.recency.retain(|value| *value != index);
    }

    fn next_visible_block(&self, index: usize) -> Option<&Block> {
        self.blocks
            .iter()
            .enumerate()
            .skip(index + 1)
            .find(|(index, _)| self.block_is_drawn(*index))
            .map(|(_, block)| block)
    }

    fn ensure_layout(&mut self) {
        if self.layout_valid {
            return;
        }
        self.offsets.clear();
        self.offsets.reserve(self.blocks.len() + 1);
        let mut total = 0usize;
        self.offsets.push(0);
        let mut remaining = 0usize;
        for index in 0..self.blocks.len() {
            let mut height = self.animated_height(index);
            match self.blocks[index].group {
                GroupRole::Head { members } => {
                    let extent = (index + 1..(index + members + 1).min(self.blocks.len()))
                        .map(|member| self.animated_height(member))
                        .sum::<usize>();
                    remaining = (extent as f32 * self.disclosure_progress(index)).ceil() as usize;
                }
                GroupRole::Member { .. } => {
                    height = height.min(remaining);
                    remaining -= height;
                }
                GroupRole::Solo => {}
            }
            total += height;
            self.offsets.push(total);
        }
        self.layout_valid = true;
    }

    fn animated_height(&self, index: usize) -> usize {
        if !self.block_is_drawn(index) {
            return 0;
        }
        let full = match self.heights[index] {
            UNMEASURED => self.estimate_height(index),
            measured => measured as usize,
        };
        let block = &self.blocks[index];
        if block.group.is_head() || !self.disclosures.contains_key(&block.id) {
            return full;
        }
        let closed = 1
            + gap_after(block, self.next_visible_block(index))
            + usize::from(block.pending_permission);
        closed.min(full)
            + (full.saturating_sub(closed) as f32 * self.disclosure_progress(index)).ceil() as usize
    }

    /// Estimate the height of an unmeasured block from its title and body shape.
    ///
    /// The estimate only has to be close: a block is measured exactly the first
    /// time it is rendered. What it must not do is disagree with the renderer
    /// about *shape*, or scrolling a session of dense rows would jump as they
    /// came into view.
    fn estimate_height(&self, index: usize) -> usize {
        let Some(block) = self.blocks.get(index) else {
            return 1;
        };
        // A folded member contributes nothing; the head reports it instead.
        if !self.block_is_drawn(index) {
            return 0;
        }
        let next = self.next_visible_block(index);
        let gap = gap_with_body(block, next, self.body_is_drawn(index));
        let available = self.width.max(8);
        let dense = is_dense_row(block.kind);
        let open = if dense {
            self.body_is_drawn(index)
        } else {
            block.is_open()
        };
        // The header is truncated to one row, and a message has none at all —
        // its first line *is* the message.
        let headerless = is_headerless(block);
        let header = usize::from(!headerless);
        // The fold's own rows are drawn by the renderer around the block's
        // content: the title above it when this block opens an open section,
        // and the rule below it when this block owns the control, each with the
        // air that follows it. An estimate that forgot them would lay the tail
        // out short of what is drawn, and the scroll would not reach the bottom.
        let title = block.fold.as_ref().is_some_and(TurnFold::draws_title);
        let furniture = fold_title_rows(title) + fold_divider_rows(block);
        let body_lines = if block.group.is_head() {
            0
        } else if open && !block.details.is_empty() {
            // The captured source, not the provider's short summary, is what
            // an expanded activity renders. Estimate without decoding offscreen
            // JSON; escaped newlines allow for known output envelopes.
            block
                .details
                .iter()
                .map(|detail| {
                    let source = detail.source();
                    1 + source.matches('\n').count()
                        + source.matches("\\n").count()
                        + source.len() / available
                })
                .sum::<usize>()
                + block.details.len().saturating_sub(1)
        } else if block.body.is_empty() {
            0
        } else if block.reasoning_window && open && next.is_none() {
            // The window is capped, which is the whole point: the rows it shows
            // are the newest ones, and how many of them a thought fills is
            // markdown's business. A count of the source lines is an upper
            // bound — markdown joins soft-wrapped lines, it does not split them
            // — and an unmeasured row must never be estimated *shorter* than it
            // draws, or the tail would sit over the rows it should be showing.
            let explicit = block.body.matches('\n').count() + 1;
            (explicit + block.body.len() / available.max(1) / 2).min(STREAMING_WINDOW_LINES)
        } else if dense && !open {
            0
        } else if open || block.streaming {
            // Count newlines plus a wrap allowance per line.
            let explicit = block.body.matches('\n').count() + 1;
            let wrap_allowance = block.body.len() / available.max(1);
            explicit + wrap_allowance / 2
        } else {
            COLLAPSED_BODY_LINES.min(block.body.matches('\n').count() + 1)
        };
        let status = usize::from(block.failed && !dense && block.kind != TimelineRowKind::Error)
            + usize::from(block.pending_permission)
            + usize::from(block.runtime_attribution.is_some() && block.expanded);
        (furniture + header + body_lines + status + gap).max(1)
    }

    /// Measure one block precisely and cache the height.
    fn measure(&mut self, index: usize, theme: &TuiTheme, strings: Strings) -> u32 {
        let key = self.keys.get(index).copied().unwrap_or_default();
        if self.heights.get(index).copied() != Some(UNMEASURED) {
            self.stats.measure_hits += 1;
            return self.heights[index];
        }
        let rendered = self.render_block(index, theme, strings);
        let height = rendered.height as u32;
        if let Some(slot) = self.heights.get_mut(index) {
            *slot = height;
        }
        self.stats.blocks_measured += 1;
        self.store_rendered(index, rendered);
        let _ = key;
        self.layout_valid = false;
        height
    }

    fn store_rendered(&mut self, index: usize, rendered: RenderedBlock) {
        self.rendered.insert(index, rendered);
        self.recency.retain(|value| *value != index);
        self.recency.push(index);
        while self.recency.len() > RENDER_CACHE_BLOCKS {
            let evicted = self.recency.remove(0);
            self.rendered.remove(&evicted);
        }
    }

    /// Advance the incremental renderer of a block that is still arriving.
    ///
    /// A block that is not streaming has no live renderer: its body takes the
    /// ordinary path, which is also what the final, non-streaming render of a
    /// finished answer uses — so the last frame of a stream and a reload of the
    /// same session agree exactly.
    fn refresh_stream(
        &mut self,
        block: &Block,
        theme: &TuiTheme,
        strings: Strings,
        width: usize,
        prose: Style,
    ) {
        if !block.streaming || !is_markdown(block.kind) {
            self.live.remove(&block.id);
            return;
        }
        let renderer = self.live.entry(block.id.clone()).or_default();
        let source = renderer.source();
        if !block.body.starts_with(source) {
            // The block was rewritten rather than appended to (an edit, or a
            // reconnect that replayed it): start the render over.
            *renderer = crate::markdown::StreamingMarkdown::new();
        }
        if block.body.len() > renderer.source().len() {
            let delta = block.body[renderer.source().len()..].to_string();
            renderer.push(&delta, theme, width, strings, prose);
        }
    }

    fn render_block(&mut self, index: usize, theme: &TuiTheme, strings: Strings) -> RenderedBlock {
        let Some(mut block) = self.blocks.get(index).cloned() else {
            return RenderedBlock::default();
        };
        if !self.block_is_drawn(index) {
            return RenderedBlock::default();
        }
        // A closing group's children remain mounted until its reveal ends.
        if let GroupRole::Member { visible, .. } = &mut block.group {
            *visible = true;
        }
        let body_open = self.body_is_drawn(index);
        // The successor decides the separator, so a run of tool calls renders
        // as a list rather than as a stack of sections.
        let next = self.next_visible_block(index).cloned();
        let prose = if matches!(block.kind, TimelineRowKind::UserMessage) {
            theme.base()
        } else {
            theme.prose()
        };
        let body_width = body_width_for(&block, self.width, strings);
        // Only a body that will be drawn is worth parsing as it arrives: a
        // dense row shows one live line, and re-rendering the whole thought to
        // keep a renderer it never reads is work the session does not need.
        // A live window always draws, so its renderer is always advanced —
        // which is what keeps a running thought's cost proportional to what is
        // on screen rather than to what has been thought.
        let body_shown = !block.body.is_empty()
            && (!is_dense_row(block.kind)
                || body_open
                || is_live_thinking_window(&block, next.is_none()));
        if body_shown {
            self.refresh_stream(&block, theme, strings, body_width, prose);
        } else {
            self.live.remove(&block.id);
        }
        // Borrowed after the renderer has been advanced, so a delta never has
        // to copy the rows that are already settled.
        let streamed = self.live.get(&block.id).map(|live| live.rendered());
        self.stats.blocks_rendered += 1;
        // A run of rows from one runtime names it once, on the row where it
        // starts — or where it changes.
        let show_attribution = block.runtime_attribution.is_some()
            && block.kind != TimelineRowKind::UserMessage
            && self
                .blocks
                .get(index.wrapping_sub(1))
                .is_none_or(|previous| {
                    !is_dense_row(previous.kind)
                        || previous.runtime_attribution != block.runtime_attribution
                });
        render_block_inner(
            &block,
            next.as_ref(),
            streamed,
            theme,
            self.width,
            strings,
            self.last_selected == Some(index),
            show_attribution,
            self.phase,
            body_open,
            self.pictures,
        )
    }

    /// Tell the transcript which block is current.
    ///
    /// Only the two affected blocks are invalidated, so moving the cursor costs
    /// two re-renders rather than a repaint of the whole cache.
    pub fn set_selected(&mut self, selected: Option<usize>) {
        if self.last_selected == selected {
            return;
        }
        if let Some(previous) = self.last_selected {
            self.invalidate(previous);
        }
        if let Some(next) = selected {
            self.invalidate(next);
        }
        self.last_selected = selected;
    }

    /// Index of the block containing display line `line`.
    pub fn block_at_line(&mut self, line: usize) -> Option<usize> {
        self.ensure_layout();
        if self.blocks.is_empty() {
            return None;
        }
        // `partition_point` on the prefix sums finds the last block starting at
        // or before `line`.
        let index = self.offsets.partition_point(|offset| *offset <= line);
        Some(index.saturating_sub(1).min(self.blocks.len() - 1))
    }

    /// Display line where `index` starts.
    pub fn line_of_block(&mut self, index: usize) -> usize {
        self.ensure_layout();
        self.offsets.get(index).copied().unwrap_or(0)
    }

    /// The display line of the block's own row: the one its mark is drawn on.
    ///
    /// A block does not always begin on its own row. A turn's fold draws its
    /// rows above the block's content — the title over the work the block
    /// opens — so the row carrying the block's disclosure sits below them, and
    /// the block's first line says "this turn's work" rather than "this run".
    /// A message with no header has no such row at all. Asking here keeps the
    /// renderer the single authority on where a gesture landed, exactly as
    /// [`Self::fold_control_lines`] does for the turn's own rows.
    pub fn block_header_line(&mut self, index: usize) -> Option<usize> {
        let block = self.blocks.get(index)?;
        if is_headerless(block) {
            return None;
        }
        let title = block.fold.as_ref().is_some_and(TurnFold::draws_title);
        Some(self.line_of_block(index) + fold_title_rows(title))
    }

    /// The display lines of `index` that toggle its turn's fold.
    ///
    /// An open section is bracketed by two rows that do the same thing — the
    /// title above the work and the rule below it — and a shut one by a single
    /// row, where the title *is* the control. Deriving them here keeps the
    /// renderer the single authority on where a gesture landed: the pointer
    /// handler asks for the rows rather than re-deriving the layout, so the two
    /// cannot disagree about what was pressed.
    pub fn fold_control_lines(&mut self, index: usize) -> Vec<usize> {
        let Some(fold) = self.blocks.get(index).and_then(|block| block.fold.as_ref()) else {
            return Vec::new();
        };
        let (title, control) = (fold.draws_title(), fold.is_control());
        if !title && !control {
            return Vec::new();
        }
        let header = usize::from(self.blocks.get(index).is_some_and(|b| !is_headerless(b)));
        let start = self.line_of_block(index);
        let mut lines = Vec::with_capacity(2);
        if title {
            lines.push(start);
        }
        if control {
            lines.push(start + fold_title_rows(title) + header);
        }
        lines
    }

    /// Materialise the blocks covering `[scroll.offset, scroll.offset + height)`.
    ///
    /// Returns the visible lines. Blocks outside the window are neither
    /// measured nor rendered, which is what keeps scrolling independent of
    /// history length.
    pub fn visible_lines(
        &mut self,
        scroll: ScrollState,
        height: usize,
        theme: &TuiTheme,
        strings: Strings,
    ) -> Vec<Line<'static>> {
        self.viewport_rows = height;
        self.window_marks.clear();
        if height == 0 || self.blocks.is_empty() {
            return Vec::new();
        }
        self.set_selected(scroll.selected);
        // The tail is measured whichever way the reader is scrolling: the
        // bottom of the transcript is what an offset is clamped against, and
        // an estimate of an undrawn block would clamp it in the wrong place.
        self.measure_tail(height, theme, strings);
        self.ensure_layout();
        let total = self.offsets.last().copied().unwrap_or(0);
        let bottom = total.saturating_sub(height);
        let offset = if scroll.follow {
            bottom
        } else {
            scroll.offset.min(bottom)
        };
        let end = (offset + height).min(total);
        self.scroll_offset = offset;

        let mut lines = Vec::with_capacity(height);
        // The pictures of the rows this window draws, in its own coordinates:
        // a mark on a row the window scrolled past is not placed at all, and a
        // picture has no clip rect, so it may only be drawn by a caller that
        // knows which rows the window actually holds.
        let mut window_marks = Vec::new();
        // Find the first block whose range intersects the window.
        let first = self
            .offsets
            .partition_point(|value| *value <= offset)
            .saturating_sub(1);
        let mut index = first;
        while index < self.blocks.len() && lines.len() < height {
            let (_, block_end_before) = self.block_range(index);
            if block_end_before <= offset {
                index += 1;
                continue;
            }
            // Measure only the blocks the viewport actually touches.
            self.measure(index, theme, strings);
            let (block_start, block_end) = self.block_range(index);
            let skip = offset.saturating_sub(block_start);
            let take = block_end.min(end).saturating_sub(block_start.max(offset));
            if take == 0 {
                index += 1;
                continue;
            }
            if let Some(rendered) = self.rendered.get(&index) {
                for (row, line) in rendered.lines.iter().enumerate().skip(skip).take(take) {
                    if let Some(mark) = rendered.mark.as_ref().filter(|mark| mark.line == row) {
                        window_marks.push(WindowMark {
                            row: lines.len(),
                            column: mark.column,
                            identity: mark.identity.clone(),
                        });
                    }
                    lines.push(self.disclosure_line(index, row, line.clone(), theme));
                }
            } else {
                // Not resident: rebuild it for this frame.
                let rendered = self.render_block(index, theme, strings);
                for (row, line) in rendered.lines.iter().enumerate().skip(skip).take(take) {
                    if let Some(mark) = rendered.mark.as_ref().filter(|mark| mark.line == row) {
                        window_marks.push(WindowMark {
                            row: lines.len(),
                            column: mark.column,
                            identity: mark.identity.clone(),
                        });
                    }
                    lines.push(self.disclosure_line(index, row, line.clone(), theme));
                }
                self.store_rendered(index, rendered);
            }
            index += 1;
        }
        self.window_marks = window_marks;
        if lines.is_empty() && !self.blocks.is_empty() && offset < total {
            self.stats.idle_frames += 1;
        }
        lines
    }

    fn disclosure_line(
        &self,
        index: usize,
        row: usize,
        mut line: Line<'static>,
        theme: &TuiTheme,
    ) -> Line<'static> {
        let mut progress =
            self.parents[index].map_or(1.0, |parent| self.disclosure_progress(parent));
        if row > 0 && self.disclosures.contains_key(&self.blocks[index].id) {
            progress *= self.disclosure_progress(index);
        }
        if progress < 1.0 {
            for span in &mut line.spans {
                let color = span
                    .style
                    .fg
                    .or(line.style.fg)
                    .unwrap_or(theme.roles.foreground);
                span.style = span.style.fg(theme.fade(color, progress));
            }
        }
        line
    }

    /// Materialise the visible lines with every search match inverted.
    ///
    /// Highlighting runs on the rendered line rather than on the block source,
    /// so a phrase that survives markdown rendering (emphasis split across
    /// spans, a wrapped paragraph) still lights up exactly where it is drawn.
    pub fn visible_lines_highlighted(
        &mut self,
        scroll: ScrollState,
        height: usize,
        theme: &TuiTheme,
        strings: Strings,
        pattern: Option<&crate::search::SearchPattern>,
        highlight: Style,
    ) -> Vec<Line<'static>> {
        let lines = self.visible_lines(scroll, height, theme, strings);
        match pattern {
            Some(pattern) => lines
                .into_iter()
                .map(|line| highlight_line(line, pattern, highlight))
                .collect(),
            None => lines,
        }
    }

    /// The user prompt pinned above the viewport, when one applies.
    ///
    /// Only the reader's own messages pin. An Agent message is not a landmark:
    /// it is the thing being read, and pinning it would cover the content the
    /// header is supposed to head. The header draws the real prompt block,
    /// truncated to its pinned height, and it is cut at the first row the body
    /// below still owns: the prompt is drawn once, as a header for the rows that
    /// have scrolled past and as a row of the transcript for the rows that have
    /// not, whether or not the reader is following the tail.
    ///
    /// `band` is the height the transcript band is drawn in, header included.
    pub fn sticky_header(
        &mut self,
        scroll: ScrollState,
        band: usize,
        theme: &TuiTheme,
        strings: Strings,
    ) -> Option<StickyHeader> {
        // A header is chrome above the transcript, not instead of it: whatever
        // it draws, the band keeps room for the body underneath.
        let viewport = band.saturating_sub(MAX_STICKY_ROWS + STICKY_GAP_ROWS);
        if viewport < 3 || self.blocks.is_empty() {
            return None;
        }
        if scroll.follow {
            // The prompt that is about to be pinned sits just above the
            // viewport, and a header can only be clipped from a block that has
            // been measured.
            self.measure_tail(band, theme, strings);
        }
        self.ensure_layout();
        let total = self.offsets.last().copied().unwrap_or(0);
        // The first row the body shows when no header is in the way. A header
        // takes its rows off the top of the band, so the row the body actually
        // starts on is this one or lower — never higher — which is what makes
        // this line the safe place to cut the header at, whatever the header
        // itself turns out to be.
        let window_top = if scroll.follow {
            total.saturating_sub(band)
        } else {
            scroll.offset.min(total.saturating_sub(band))
        };
        if window_top == 0 {
            return None;
        }
        // The last prompt the reader has scrolled past.
        let mut candidate = None;
        for index in 0..self.blocks.len() {
            let start = self.offsets.get(index).copied().unwrap_or(0);
            if start >= window_top {
                break;
            }
            if self.blocks[index].kind == TimelineRowKind::UserMessage && self.is_sticky(index) {
                candidate = Some(index);
            }
        }
        let index = candidate?;
        let full_height = self.measure(index, theme, strings) as usize;
        self.ensure_layout();
        let start = self.offsets.get(index).copied().unwrap_or(0);
        // The header shrinks one row per row scrolled past, but never below the
        // height it would have inline-truncated to, so the question stays
        // readable however far the reader has scrolled — and never past the rows
        // that have actually left the window, because those still inside it are
        // drawn by the transcript below.
        let scroll_past = window_top.saturating_sub(start);
        let floor = full_height.clamp(1, MAX_STICKY_ROWS);
        let height = full_height
            .saturating_sub(scroll_past)
            .max(floor)
            .min(viewport.saturating_sub(1))
            .min(scroll_past);
        let rendered = self.rendered.get(&index)?.clone();
        // The block's own trailing blank rows are the separator before the next
        // block, not content: a header clipped down to them would reserve a row
        // to show nothing, so the clip stops at the last row with text on it —
        // plus the rows of a band that closes the block, because the padding
        // inside the reader's own box belongs to the box and a pinned prompt
        // missing its floor would look like a rendering fault.
        let last_text = rendered
            .plain
            .iter()
            .rposition(|line| !line.trim().is_empty())
            .map_or(0, |last| last + 1);
        let content_rows = last_text
            + rendered
                .lines
                .get(last_text..)
                .unwrap_or_default()
                .iter()
                .take_while(|line| line.style.bg == Some(theme.roles.surface_raised))
                .count();
        if content_rows == 0 {
            return None;
        }
        let end = height.min(content_rows);
        let lines = rendered.lines.get(..end).unwrap_or_default().to_vec();
        if lines.is_empty() {
            return None;
        }
        Some(StickyHeader {
            block: index,
            lines,
        })
    }

    /// Whether a prompt still pins when it has scrolled away.
    ///
    /// A block the reader deliberately expanded shows all of itself inline, so
    /// pinning a truncated copy of it would be a second, worse copy.
    fn is_sticky(&self, index: usize) -> bool {
        self.blocks
            .get(index)
            .is_some_and(|block| !(block.collapsible && block.expanded))
    }

    /// Measure blocks backwards from the end until `height` lines are covered.
    fn measure_tail(&mut self, height: usize, theme: &TuiTheme, strings: Strings) {
        if self.disclosures.is_empty() {
            // At rest, measure the tail in one pass and rebuild prefix sums
            // once. Only an active reveal needs partially clipped extents.
            let mut covered = 0;
            for index in (0..self.blocks.len()).rev() {
                if covered >= height {
                    break;
                }
                if !self.blocks[index].is_hidden() {
                    covered += self.measure(index, theme, strings) as usize;
                }
            }
            return;
        }
        let mut covered = 0usize;
        let mut index = self.blocks.len();
        while index > 0 && covered < height {
            index -= 1;
            let (start, end) = self.block_range(index);
            if start == end {
                continue;
            }
            if self.heights.get(index).copied() == Some(UNMEASURED) {
                self.measure(index, theme, strings);
            }
            let (start, end) = self.block_range(index);
            covered += end - start;
        }
    }

    fn block_range(&mut self, index: usize) -> (usize, usize) {
        self.ensure_layout();
        let start = self.offsets.get(index).copied().unwrap_or(0);
        let end = self.offsets.get(index + 1).copied().unwrap_or(start);
        (start, end)
    }

    /// Plain text of the whole transcript, used by copy and search.
    pub fn plain_text(&mut self) -> String {
        let mut output = String::new();
        for index in 0..self.blocks.len() {
            if self.blocks[index].group.is_head() {
                continue;
            }
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&block_plain_text(&self.blocks[index]));
        }
        output
    }

    /// Plain text of one block.
    pub fn block_text(&self, index: usize) -> Option<String> {
        let block = self.blocks.get(index)?;
        if let GroupRole::Head { members } = block.group {
            Some(
                self.blocks[index + 1..index + 1 + members]
                    .iter()
                    .map(block_plain_text)
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            )
        } else {
            Some(block_plain_text(block))
        }
    }

    /// The plain text of a range of display lines, with the chrome each line
    /// carries before its text.
    ///
    /// Unlike [`Transcript::visible_lines`] this is not restricted to the
    /// viewport: a selection can be dragged past the edge of the screen, and
    /// the copy must contain what the reader dragged over rather than what
    /// happened to be on screen when they let go.
    pub fn plain_lines(
        &mut self,
        start: usize,
        end: usize,
        theme: &TuiTheme,
        strings: Strings,
    ) -> Vec<PlainLine> {
        if end <= start || self.blocks.is_empty() {
            return Vec::new();
        }
        self.ensure_layout();
        let total = self.offsets.last().copied().unwrap_or(0);
        let start = start.min(total);
        let end = end.min(total);
        if end <= start {
            return Vec::new();
        }
        let mut lines = Vec::with_capacity(end - start);
        let mut index = self
            .offsets
            .partition_point(|value| *value <= start)
            .saturating_sub(1);
        while index < self.blocks.len() && lines.len() < end - start {
            self.measure(index, theme, strings);
            let (block_start, block_end) = self.block_range(index);
            if block_end <= start {
                index += 1;
                continue;
            }
            let skip = start.saturating_sub(block_start);
            let take = block_end.min(end).saturating_sub(block_start.max(start));
            if take > 0 {
                let rendered = match self.rendered.get(&index) {
                    Some(rendered) => rendered.clone(),
                    None => {
                        let rendered = self.render_block(index, theme, strings);
                        self.store_rendered(index, rendered.clone());
                        rendered
                    }
                };
                for line in rendered.plain.iter().skip(skip).take(take) {
                    lines.push(PlainLine {
                        text: line.clone(),
                        // Every row puts its text behind the same chrome. A
                        // boxed block's own edge and padding are the marker
                        // column and the pad column, the two cells no row's text
                        // ever occupies, so the box needs no column of its own.
                        chrome: chrome::PAD_LEFT,
                    });
                }
            }
            index += 1;
        }
        lines
    }

    /// Metadata text for one block, for the `Shift+Y` copy action.
    pub fn block_metadata(&self, index: usize) -> Option<String> {
        let block = self.blocks.get(index)?;
        let mut fields = vec![
            format!("kind={}", kind_id(block.kind)),
            format!("sequence={}", block.sequence),
        ];
        if let Some(turn_id) = &block.turn_id {
            fields.push(format!("turn={turn_id}"));
        }
        if let Some(at_ms) = block.timestamp_ms {
            fields.push(format!("at={at_ms}"));
        }
        if let Some(path) = &block.file_path {
            fields.push(format!("path={path}"));
        }
        if let Some(runtime) = &block.runtime_attribution {
            fields.push(format!("runtime={runtime}"));
        }
        Some(fields.join(" "))
    }

    /// Indices of blocks whose plain text matches `query`, case-insensitively.
    pub fn search(&self, query: &str) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| {
                !block.group.is_head()
                    && (block.title.to_lowercase().contains(&query)
                        || block_body_text(block).to_lowercase().contains(&query))
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Run a compiled pattern over every block.
    ///
    /// Returns the indices of the blocks that contain at least one match and
    /// the total match count. Both are needed: the counter reports the total,
    /// while stepping moves between blocks, because scrolling to a match is a
    /// block-level operation.
    ///
    /// Collapsed and folded-away content is searched like any other: the reader
    /// searching for a phrase expects to be taken to it, not told it does not
    /// exist because it is behind a fold.
    pub fn search_blocks(&self, pattern: &crate::search::SearchPattern) -> (Vec<usize>, usize) {
        let mut blocks = Vec::new();
        let mut total = 0usize;
        for (index, block) in self.blocks.iter().enumerate() {
            if block.group.is_head() {
                continue;
            }
            let hits = pattern.count(&block.title) + pattern.count(&block_body_text(block));
            if hits > 0 {
                blocks.push(index);
                total += hits;
            }
        }
        (blocks, total)
    }

    /// Scroll offset needed to bring `index` to the top of the viewport.
    pub fn offset_of_block(&mut self, index: usize) -> usize {
        self.line_of_block(index)
    }
}

/// Paint a column range of one rendered line with `style`.
///
/// The line is rebuilt grapheme by grapheme so a wide character is never split
/// across the boundary of the band: a cell in a terminal is two columns wide
/// for CJK, and half a glyph is not a thing that can be drawn.
pub fn paint_columns(line: Line<'static>, from: usize, to: usize, style: Style) -> Line<'static> {
    if from >= to {
        return line;
    }
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 2);
    let mut column = 0usize;
    for span in line.spans {
        let mut pending: Option<(bool, String)> = None;
        for grapheme in span.content.graphemes(true) {
            let width = display_width(grapheme);
            let covered = column < to && column + width > from;
            column += width;
            match pending.as_mut() {
                Some((flag, text)) if *flag == covered => text.push_str(grapheme),
                Some((flag, text)) => {
                    // The style changes here, so the run so far is emitted and
                    // a new one starts. Order is preserved exactly.
                    spans.push(Span::styled(
                        std::mem::take(text),
                        if *flag { style } else { span.style },
                    ));
                    *flag = covered;
                    text.push_str(grapheme);
                }
                None => pending = Some((covered, grapheme.to_string())),
            }
        }
        if let Some((flag, text)) = pending {
            spans.push(Span::styled(text, if flag { style } else { span.style }));
        }
    }
    Line {
        spans,
        style: line.style,
        alignment: line.alignment,
    }
}

/// The column range of the word under `column`, or `None` over whitespace.
///
/// Word boundaries are whitespace, not punctuation: double-clicking `src/net.rs`
/// selects the whole path, which is what a reader copying a file name wants.
pub fn word_at(text: &str, column: u16) -> Option<(u16, u16)> {
    let column = usize::from(column);
    let mut cells: Vec<(usize, usize, &str)> = Vec::new();
    let mut width_so_far = 0usize;
    for grapheme in text.graphemes(true) {
        let width = display_width(grapheme);
        cells.push((width_so_far, width, grapheme));
        width_so_far += width;
    }
    let position = cells
        .iter()
        .position(|(start, width, _)| column >= *start && column < start + width)?;
    let is_word = |grapheme: &str| !grapheme.trim().is_empty();
    if !is_word(cells[position].2) {
        return None;
    }
    let mut first = position;
    while first > 0 && is_word(cells[first - 1].2) {
        first -= 1;
    }
    let mut last = position;
    while last + 1 < cells.len() && is_word(cells[last + 1].2) {
        last += 1;
    }
    let start = cells[first].0 as u16;
    let end = (cells[last].0 + cells[last].1) as u16;
    Some((start, end))
}

/// Invert every search match in one rendered line.///
/// The spans are split at the match boundaries rather than rebuilt, so the
/// glyphs, their widths and their order are exactly what the block renderer
/// produced; only the style of the matched cells changes.
pub fn highlight_line(
    line: Line<'static>,
    pattern: &crate::search::SearchPattern,
    highlight: Style,
) -> Line<'static> {
    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let ranges = pattern.ranges(&text);
    if ranges.is_empty() {
        return line;
    }
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + ranges.len());
    let mut offset = 0usize;
    for span in line.spans {
        let start = offset;
        let end = offset + span.content.len();
        offset = end;
        let mut cursor = start;
        for range in ranges
            .iter()
            .filter(|range| range.start < end && range.end > start)
        {
            let match_start = range.start.max(start);
            let match_end = range.end.min(end);
            if cursor < match_start {
                spans.push(Span::styled(
                    span.content[cursor - start..match_start - start].to_string(),
                    span.style,
                ));
            }
            if match_start < match_end {
                spans.push(Span::styled(
                    span.content[match_start - start..match_end - start].to_string(),
                    highlight,
                ));
            }
            cursor = match_end.max(cursor);
        }
        if cursor < end {
            spans.push(Span::styled(
                span.content[cursor - start..].to_string(),
                span.style,
            ));
        }
    }
    Line {
        spans,
        style: line.style,
        alignment: line.alignment,
    }
}

/// Fit a sequence of styled parts into `width` columns, dropping whole parts/// from the end before truncating the last one that fits.
fn truncate_parts(parts: Vec<(String, Style)>, width: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(parts.len());
    let mut used = 0usize;
    for (text, style) in parts {
        let text_width = display_width(&text);
        if used + text_width <= width {
            used += text_width;
            spans.push(Span::styled(text, style));
            continue;
        }
        let remaining = width.saturating_sub(used);
        if remaining > 0 {
            spans.push(Span::styled(
                truncate_to_width(&text, remaining, "…"),
                style,
            ));
        }
        break;
    }
    spans
}

/// Human-readable block body used for the copy action.
fn block_body_text(block: &Block) -> Cow<'_, str> {
    if block.details.is_empty() {
        Cow::Borrowed(&block.body)
    } else {
        Cow::Owned(tool_detail::text(&block.details))
    }
}

/// Human-readable block identity and body used for copy and text export.
fn block_plain_text(block: &Block) -> String {
    let mut output = block
        .activity
        .as_ref()
        .map_or_else(|| block.title.clone(), Activity::label);
    let body = block_body_text(block);
    if !body.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&body);
    }
    output
}

pub fn kind_id(kind: TimelineRowKind) -> &'static str {
    match kind {
        TimelineRowKind::UserMessage => "user_message",
        TimelineRowKind::AgentMessage => "agent_message",
        TimelineRowKind::Reasoning => "reasoning",
        TimelineRowKind::Plan => "plan",
        TimelineRowKind::ToolCall => "tool_call",
        TimelineRowKind::Command => "command",
        TimelineRowKind::FileOperation => "file_operation",
        TimelineRowKind::WebSearch => "web_search",
        TimelineRowKind::TodoUpdate => "todo_update",
        TimelineRowKind::Collaboration => "collaboration",
        TimelineRowKind::ImageGeneration => "image_generation",
        TimelineRowKind::GitNotice => "git_notice",
        TimelineRowKind::SystemNotice => "system_notice",
        TimelineRowKind::PermissionRequest => "permission_request",
        TimelineRowKind::PermissionResolution => "permission_resolution",
        TimelineRowKind::ElicitationRequest => "elicitation_request",
        TimelineRowKind::ElicitationResolution => "elicitation_resolution",
        TimelineRowKind::Retry => "retry",
        TimelineRowKind::Error => "error",
    }
}

/// The icon and accent used for a block kind.
/// Localised label for a block kind.
pub fn kind_label(kind: TimelineRowKind, strings: Strings) -> &'static str {
    match kind {
        TimelineRowKind::UserMessage => strings.transcript_user(),
        TimelineRowKind::AgentMessage => strings.transcript_agent(),
        TimelineRowKind::Reasoning => strings.transcript_thinking(),
        TimelineRowKind::Plan => strings.transcript_plan(),
        TimelineRowKind::ToolCall => strings.transcript_tool(),
        TimelineRowKind::Command => strings.transcript_command(),
        TimelineRowKind::FileOperation => strings.transcript_file_change(),
        TimelineRowKind::WebSearch => strings.transcript_web_search(),
        TimelineRowKind::TodoUpdate => strings.transcript_todo(),
        TimelineRowKind::Collaboration => strings.transcript_collaboration(),
        TimelineRowKind::ImageGeneration => strings.transcript_image(),
        TimelineRowKind::GitNotice => strings.transcript_git(),
        TimelineRowKind::SystemNotice => strings.transcript_system(),
        TimelineRowKind::PermissionRequest => strings.transcript_permission(),
        TimelineRowKind::PermissionResolution => strings.transcript_permission(),
        TimelineRowKind::ElicitationRequest => strings.transcript_elicitation(),
        TimelineRowKind::ElicitationResolution => strings.transcript_elicitation(),
        TimelineRowKind::Retry => strings.transcript_retry(),
        TimelineRowKind::Error => strings.transcript_error(),
    }
}

/// Whether a kind renders its body as Markdown.
fn is_markdown(kind: TimelineRowKind) -> bool {
    matches!(
        kind,
        TimelineRowKind::AgentMessage
            | TimelineRowKind::Reasoning
            | TimelineRowKind::Plan
            | TimelineRowKind::Error
            | TimelineRowKind::SystemNotice
            | TimelineRowKind::GitNotice
            | TimelineRowKind::TodoUpdate
    )
}

/// Render one block into display lines.
/// Chrome geometry for one transcript block.
///
/// The left edge is empty: a block is identified by what it says — the prompt
/// mark on the reader's own words, the bullet on a work item, the colour of the
/// text — not by a bar beside it. A column of colour per block is a column the
/// text does not get, and once every row is one line tall the bars of adjacent
/// blocks read as one striped edge rather than as one bar per block.
pub mod chrome {
    /// Gap between the left edge and the content.
    ///
    /// The selection pointer is drawn in the first of these columns, so marking
    /// a block never moves its text.
    pub const PAD_LEFT: usize = 2;
    /// Gap between the content and the right edge.
    pub const PAD_RIGHT: usize = 1;
    /// Blank rows inserted between blocks.
    pub const GAP: usize = 1;
    /// One connector cell and one space per disclosure level.
    pub const NEST: usize = 2;
    /// Columns the chrome consumes in total.
    pub const TOTAL: usize = PAD_LEFT + PAD_RIGHT;

    /// Content width available at a given total width.
    pub const fn content_width(width: usize) -> usize {
        width.saturating_sub(TOTAL)
    }
}

/// The glyph marking the reader's own message.
fn prompt_glyph(theme: &TuiTheme) -> &'static str {
    if theme.glyphs() == crate::theme::GlyphMode::Unicode {
        "❯"
    } else {
        ">"
    }
}

/// The glyph marking the block the cursor is on.
fn pointer_glyph(theme: &TuiTheme) -> &'static str {
    if theme.glyphs() == crate::theme::GlyphMode::Unicode {
        "▌"
    } else {
        ">"
    }
}

/// The separator used between chrome items across the interface.
pub fn chrome_separator() -> &'static str {
    "│"
}

/// Columns between a message's text and the clock on its right.
///
/// The clock is a margin note, not a column of the message: two cells are
/// enough to keep it from reading as the last word of the sentence.
const CLOCK_GAP: usize = 2;

/// Columns between the clock and the content's right edge.
///
/// One, so the clock is not glued to the edge of the band it sits in — the same
/// cell the session list leaves after its own age column.
const CLOCK_EDGE: usize = 1;

/// The edge drawn down the left of the reader's own surfaces.
///
/// The transcript's message band and the composer's writing surface are one
/// object — what the reader wrote and what they are writing — so both are
/// marked by the same cell: one column of the heavy vertical the rail
/// vocabulary already draws, in the green every shipped theme resolves
/// `chart-2` to. It is the one rail on the page that belongs to the reader
/// rather than to the Agent.
///
/// A box spends a cell it already has on it rather than a new one: the message
/// draws the edge in the column every block spends on its marker, the composer
/// where its own band starts, so the two land in the same column of the page.
/// The padding cell behind it is the same cell on both — the margin every block
/// spends, which the composer insets its draft by as well — and the prompt mark
/// and the words under it therefore start on one column whether the reader is
/// reading what they wrote or writing what they will.
pub(crate) fn box_edge(theme: &TuiTheme) -> Span<'static> {
    Span::styled(
        crate::glyphs::accent_bar(crate::glyphs::GlyphTier::of(theme)).to_string(),
        Style::default().fg(theme.roles.accent_success),
    )
}

/// Columns the reader's own box spends on the edge that marks it.
///
/// One cell, on the left of the band: the same cell the composer draws its own
/// edge in, so the message and the box it was typed into are recognisably the
/// same object at two moments. The edge is drawn from the theme, so the *width*
/// is the only thing a layout can be told in advance — and every rail glyph the
/// vocabulary draws is one column, which is what makes this a constant.
pub(crate) const BOX_EDGE_WIDTH: usize = 1;

/// Empty rows the reader's own message keeps above and below its text.
///
/// The composer's padding, row for row. A message drawn as one row of text on a
/// band is a highlight behind a sentence; the box the reader wrote into has
/// room in it, and the transcript draws the message they sent rather than a
/// compressed copy of it.
const BOX_PAD_ROWS: usize = 1;

/// The clock a message carries on its right, if it has one.
///
/// Only the conversation is stamped. A work item is a row of activity whose
/// place in the transcript is already given by the messages around it, and a
/// clock on each of forty tool calls is forty labels saying the same minute.
fn clock_label(block: &Block, strings: Strings) -> Option<String> {
    if !matches!(
        block.kind,
        TimelineRowKind::UserMessage | TimelineRowKind::AgentMessage
    ) {
        return None;
    }
    let at_ms = block.timestamp_ms.filter(|at_ms| *at_ms > 0)?;
    let label = crate::text::format_clock(at_ms, strings.locale);
    (!label.is_empty()).then_some(label)
}

/// Columns a block reserves on its right for the clock, gutter included.
///
/// The body is wrapped to what is left, which is what keeps the clock off the
/// text it belongs to: a label drawn over the words of a message is worse than
/// no label at all.
fn clock_columns(block: &Block, strings: Strings) -> usize {
    clock_label(block, strings)
        .map(|label| display_width(&label) + CLOCK_GAP + CLOCK_EDGE)
        .unwrap_or(0)
}

/// The columns a block's body is wrapped to inside the transcript band.
///
/// Shared by the incremental streaming renderer and the block renderer: a
/// streamed answer is laid out from a frozen prefix, so the two have to agree
/// on the width or the last settled line and the first live one would break in
/// different places.
pub fn body_width_for(block: &Block, width: usize, strings: Strings) -> usize {
    // The reader's own message pays for its prompt mark. Its edge and the
    // padding cell behind it are the columns every block spends on its marker
    // and its margin, which are already outside this width; a message of either
    // kind pays for the clock, whatever its label needs.
    let boxed = if band_fills_row(block) {
        crate::glyphs::PROMPT_ARROW_WIDTH
    } else {
        0
    };
    chrome::content_width(width.max(8))
        .max(8)
        .saturating_sub(boxed)
        .saturating_sub(clock_columns(block, strings))
        .saturating_sub(group_indent(block))
        .saturating_sub(detail_indent(block))
        .max(8)
}

fn group_indent(block: &Block) -> usize {
    if matches!(block.group, GroupRole::Member { .. }) {
        chrome::NEST
    } else {
        0
    }
}

/// Whether a block draws its body without a header row above it.
///
/// A message *is* its text: a "You" or "Agent" row above every one of them
/// spends a row of the transcript saying what the rail colour already says. The
/// renderer and the height estimate both need this, and a disagreement about it
/// is a row of drift between what is drawn and what is scrolled to.
fn is_headerless(block: &Block) -> bool {
    matches!(
        block.kind,
        TimelineRowKind::UserMessage | TimelineRowKind::AgentMessage
    ) && !block.body.is_empty()
}

/// The rows a block spends on its turn fold's title, above its own content.
///
/// Two when the block opens an open section: the title row itself and the air
/// under it, which is what stops the duration from reading as a heading of the
/// work below. A shut section draws no title — the control *is* the title — and
/// a block that does not open the section draws none either.
fn fold_title_rows(title: bool) -> usize {
    usize::from(title) * 2
}

/// The rows a block spends on the rule that closes its turn's process section.
///
/// Two, for the same reason: the rule and the air under it, so the answer is
/// not read as one more line of the work above.
fn fold_divider_rows(block: &Block) -> usize {
    usize::from(block.fold.as_ref().is_some_and(TurnFold::is_control)) * 2
}

fn detail_indent(block: &Block) -> usize {
    if !block.group.is_head() && (is_tool_item(block.kind) || block.activity.is_some()) {
        chrome::NEST
    } else {
        0
    }
}

/// Whether a block's body is drawn on a band that fills the whole row.
///
/// The reader's own message is an object rather than an entry: the band is its
/// container, and a container that stopped where the words did would read as a
/// highlight behind a sentence instead of as the block the reader wrote into.
fn band_fills_row(block: &Block) -> bool {
    block.kind == TimelineRowKind::UserMessage
}

/// One empty row of a banded block: the padding above and below the text.
///
/// It is a row of the box rather than a gap between blocks, which is why it
/// carries the edge and the band to the content edge, and why it keeps an empty
/// plain line — the copy of a message is the message, not its padding. The row
/// holds nothing but that padding, so the box's own edge is the first thing
/// after the chrome and the band fills the rest.
///
/// The edge is the only mark it draws. The padding is air the box keeps, not a
/// row of the message, so there is nothing here for a cursor to take: the
/// selection lights the box as an object instead.
fn band_row(content_width: usize, theme: &TuiTheme) -> Line<'static> {
    let band = Style::default().bg(theme.roles.surface_raised);
    let mut spans = Vec::new();
    close_body_row(
        &mut spans,
        0,
        content_width,
        Some(band),
        None,
        Style::default(),
    );
    row_line(box_edge(theme), spans).style(band)
}

/// Light the reader's own box, which is what the cursor on it looks like.
///
/// Every other block marks its current row with the pointer in the margin — a
/// swap the reader reads because the cell underneath was blank. The box's margin
/// already carries the edge that says whose message this is, and the box is a
/// surface rather than a titled section with a header to lift, so the cursor is
/// spent on the whole object: the reader sees the message they are on light up
/// rather than a glyph appear in a column that already meant something else.
///
/// Only the cells are repainted; the row's own style keeps saying the block is a
/// band, because a pinned prompt is clipped by reading exactly that.
fn lift_box(lines: &mut [Line<'static>], theme: &TuiTheme) {
    let lit = Some(theme.roles.surface_highlight);
    for line in lines {
        for span in &mut line.spans {
            span.style.bg = lit;
        }
    }
}

/// Whether a block kind is a dense, foldable work item.
///
/// These are the blocks that get bullets and participate in grouping: a session
/// can contain dozens of them in a row, and rendering each one as a full block
/// buries the conversation they belong to.
pub fn is_work_item(kind: TimelineRowKind) -> bool {
    matches!(
        kind,
        TimelineRowKind::ToolCall
            | TimelineRowKind::Command
            | TimelineRowKind::FileOperation
            | TimelineRowKind::WebSearch
            | TimelineRowKind::Reasoning
            | TimelineRowKind::TodoUpdate
            | TimelineRowKind::Collaboration
            | TimelineRowKind::ImageGeneration
    )
}

/// The leading mark a work item carries on its first line.
///
/// `live` says the block is drawing a window under the header, so the
/// disclosure points at the rows it has rather than away from them.
fn bullet(block: &Block, live: bool, theme: &TuiTheme) -> Option<(&'static str, Style)> {
    if !is_work_item(block.kind) {
        return None;
    }
    let tier = crate::glyphs::GlyphTier::of(theme);
    let glyph = if block.failed {
        crate::glyphs::ballot_x(tier)
    } else {
        crate::glyphs::disclosure(block.expanded || live, tier)
    };
    let color = if block.failed {
        theme.roles.danger
    } else if block.streaming {
        theme.roles.accent_running
    } else {
        theme.roles.gray
    };
    Some((glyph, Style::default().fg(color)))
}

/// How a block participates in a dense run of work items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GroupRole {
    /// Renders on its own.
    #[default]
    Solo,
    /// A persistent summary control for the following members.
    Head { members: usize },
    /// A child header, with its own detail disclosure when the group is visible.
    Member { visible: bool, last: bool },
}

impl GroupRole {
    pub fn is_head(self) -> bool {
        matches!(self, Self::Head { .. })
    }

    pub fn is_hidden(self) -> bool {
        matches!(self, Self::Member { visible: false, .. })
    }
}
/// Render one block into display lines.
///
/// Every row starts with [`chrome::PAD_LEFT`] columns of margin, the first of
/// which carries the selection pointer.
pub fn render_block(
    block: &Block,
    theme: &TuiTheme,
    width: usize,
    strings: Strings,
) -> RenderedBlock {
    render_block_at_phase(block, theme, width, strings, 0)
}

/// As [`render_block`], with the chrome animation clock.
///
/// Only a live thought window reads the clock: its rail is a wave whose crest
/// travels down the window, and the phase is where the crest is.
pub fn render_block_at_phase(
    block: &Block,
    theme: &TuiTheme,
    width: usize,
    strings: Strings,
    phase: u32,
) -> RenderedBlock {
    render_block_styled(block, theme, width, strings, false, phase)
}

/// Whether a block reads as one dense row rather than a titled section.
///
/// Work items and notices are the bulk of a session and the least of it: a run
/// of ten reads is evidence for a sentence, not ten sections. Each one is a
/// single row — its action and the one detail that identifies it — and the full
/// text is one keypress away in the block's detail overlay.
pub fn is_dense_row(kind: TimelineRowKind) -> bool {
    is_work_item(kind)
        || matches!(
            kind,
            TimelineRowKind::SystemNotice
                | TimelineRowKind::GitNotice
                | TimelineRowKind::Retry
                | TimelineRowKind::PermissionResolution
                | TimelineRowKind::ElicitationResolution
        )
}

/// Whether a block's first row carries the kind's name as well as its title.
///
/// Two labels are worse than one: "Agent Agent" and "Reasoning Reasoning" cost
/// a reader attention on every row to say nothing. A work item's title is the
/// action it took, and the rail already says it is work.
fn shows_kind_label(kind: TimelineRowKind, title: &str, label: &str) -> bool {
    if is_work_item(kind) {
        return false;
    }
    !title.eq_ignore_ascii_case(label) && !title.to_lowercase().starts_with(&label.to_lowercase())
}

/// The one line a dense row shows beside its title.
///
/// This is the detail that identifies the row — a path, a match count, the
/// first line of output — not the beginning of a report: the body stays behind
/// the fold. A tool call is the case worth naming: its body is a payload, and
/// printing the payload turns every row into a wall of JSON, so the *action* is
/// what the row shows.
fn dense_summary(block: &Block) -> Option<String> {
    if let Some(activity) = &block.activity {
        return (!activity.target().is_empty()).then(|| activity.target().to_string());
    }
    if block.kind == TimelineRowKind::Reasoning {
        return None;
    }
    if let Some(path) = &block.file_path {
        return Some(path.clone());
    }
    let body = block.body.trim();
    if body.is_empty() {
        return None;
    }
    let first = match is_tool_item(block.kind) {
        true => tool_action(body),
        false => body
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .to_string(),
    };
    let first = first.trim();
    if first.is_empty() {
        return None;
    }
    // A summary that only repeats the title says nothing.
    (!first.eq_ignore_ascii_case(block.title.trim())).then(|| first.to_string())
}

/// Whether a kind's body is a tool payload rather than prose.
fn is_tool_item(kind: TimelineRowKind) -> bool {
    matches!(
        kind,
        TimelineRowKind::ToolCall
            | TimelineRowKind::Command
            | TimelineRowKind::FileOperation
            | TimelineRowKind::WebSearch
            | TimelineRowKind::ImageGeneration
    )
}

/// The action inside a tool payload, in the terms a reader thinks in.
///
/// A call arrives as JSON — `{"command":"cargo test -p vibex-tui"}` — and the
/// interesting part is one string field. The fields are tried in the order that
/// answers "what is it doing": the command it runs, the file it touches, the
/// thing it looks for, then whatever prose the caller sent with it. A body that
/// is not JSON (already-rendered output, a plain command) is its own summary.
fn tool_action(body: &str) -> String {
    let trimmed = body.trim();
    if !trimmed.starts_with(['{', '[', '<']) {
        return trimmed
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .to_string();
    }
    // A call body can contain input followed by output. Decode just the first
    // value; incomplete or unknown payloads stay in the expandable details.
    let Some(Ok(value)) = serde_json::Deserializer::from_str(trimmed)
        .into_iter::<serde_json::Value>()
        .next()
    else {
        return String::new();
    };
    const FIELDS: [&str; 7] = [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
    ];
    for field in FIELDS {
        let Some(found) = value.get(field) else {
            continue;
        };
        let text = match found {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(items) => items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            other => other.to_string(),
        };
        let text = text.trim();
        if !text.is_empty() {
            return text.to_string();
        }
    }
    String::new()
}

/// The blank rows that follow a block.
///
/// Two dense rows are a list, not two sections, and a blank row between every
/// pair of them is most of a session's height. Everything else keeps the gap
/// that separates blocks into objects. A live window needs no rule of its own:
/// it is the last block there is, and the tail of a transcript always keeps its
/// gap.
pub fn gap_after(block: &Block, next: Option<&Block>) -> usize {
    gap_with_body(block, next, block.expanded)
}

/// The blank rows a block leaves under itself, before the next one is drawn.
///
/// A run of work items reads as one list, so while its rows are being drawn the
/// hair between them is what holds them together. The run is still an object
/// among the blocks around it: the summary that heads it takes the ordinary gap
/// as soon as what follows is not one of its own rows, or the group's last line
/// and the sentence that answers it would be one paragraph.
fn gap_with_body(block: &Block, next: Option<&Block>, body_open: bool) -> usize {
    let dense_run =
        is_dense_row(block.kind) && !body_open && next.is_some_and(|next| is_dense_row(next.kind));
    if dense_run { 0 } else { chrome::GAP }
}

/// As [`render_block`], with the current-block treatment.
///
/// The current block is marked rather than inverted: a full-width reversed row
/// is the heaviest possible emphasis and makes the transcript look like a
/// spreadsheet with a selected cell. A pointer in the rail plus a lifted header
/// background says the same thing at a fraction of the volume.
pub fn render_block_styled(
    block: &Block,
    theme: &TuiTheme,
    width: usize,
    strings: Strings,
    selected: bool,
    phase: u32,
) -> RenderedBlock {
    render_block_in_run(block, None, theme, width, strings, selected, phase)
}

/// As [`render_block_styled`], told what follows the block.
///
/// The run's shape decides the separator: two dense rows sit together as a run,
/// everything else is separated into an object. Passing the neighbour is what
/// lets a session of forty tool calls read as a list instead of forty sections.
pub fn render_block_in_run(
    block: &Block,
    next: Option<&Block>,
    theme: &TuiTheme,
    width: usize,
    strings: Strings,
    selected: bool,
    phase: u32,
) -> RenderedBlock {
    render_block_in_run_with_body(block, next, None, theme, width, strings, selected, phase)
}

/// As [`render_block_in_run`], with a body the caller has already rendered.
///
/// The transcript passes the live renderer's output for a block that is still
/// arriving, so a streamed answer is laid out from its frozen prefix instead of
/// being re-parsed from the first token on every delta.
#[allow(clippy::too_many_arguments)]
pub fn render_block_in_run_with_body(
    block: &Block,
    next: Option<&Block>,
    streamed: Option<&crate::markdown::RenderedMarkdown>,
    theme: &TuiTheme,
    width: usize,
    strings: Strings,
    selected: bool,
    phase: u32,
) -> RenderedBlock {
    render_block_with_attribution(
        block, next, streamed, theme, width, strings, selected, true, phase,
    )
}

/// The separator between the names in a line the runtime composed: an
/// attribution line's Agent, account and model, or a reference's account, model
/// and configuration.
const RUNTIME_ATTRIBUTION_SEPARATOR: &str = " · ";

/// A line the runtime composed out of names, with the Agent account named in
/// the reader's language.
///
/// The line is composed by the shared projection out of labels the runtime
/// wrote, and the account is the one part of it this client has a name of its
/// own for: an Agent account is a product concept, while the Agent, the model
/// and a Provider Profile are all names the reader already knows. Only the
/// account segment is touched, and only when
/// [`vibex_core::agent_account_label_hint`] recognises the runtime's own
/// wording, so a Profile whose owner named it "Agent account" is never renamed.
/// The account hint stays behind the name.
pub(crate) fn runtime_attribution_line(strings: Strings, attribution: &str) -> String {
    let account = strings.runtime_auth_agent_account();
    attribution
        .split(RUNTIME_ATTRIBUTION_SEPARATOR)
        .map(
            |segment| match vibex_core::agent_account_label_hint(segment) {
                Some("") => account.to_string(),
                Some(hint) => format!("{account} - {hint}"),
                None => segment.to_string(),
            },
        )
        .collect::<Vec<_>>()
        .join(RUNTIME_ATTRIBUTION_SEPARATOR)
}

/// As [`render_block_in_run_with_body`], told whether this row carries the
/// runtime's name.
///
/// A session's rows usually come from one runtime, and printing its name on
/// every one of them is the same three words a hundred times: it is worth a row
/// only where it *changes*, which is where the reader learns something.
#[allow(clippy::too_many_arguments)]
pub fn render_block_with_attribution(
    block: &Block,
    next: Option<&Block>,
    streamed: Option<&crate::markdown::RenderedMarkdown>,
    theme: &TuiTheme,
    width: usize,
    strings: Strings,
    selected: bool,
    show_attribution: bool,
    phase: u32,
) -> RenderedBlock {
    render_block_inner(
        block,
        next,
        streamed,
        theme,
        width,
        strings,
        selected,
        show_attribution,
        phase,
        block.expanded,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_block_inner(
    block: &Block,
    next: Option<&Block>,
    streamed: Option<&crate::markdown::RenderedMarkdown>,
    theme: &TuiTheme,
    width: usize,
    strings: Strings,
    selected: bool,
    show_attribution: bool,
    phase: u32,
    body_open: bool,
    pictures: bool,
) -> RenderedBlock {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut plain: Vec<String> = Vec::new();

    if block.is_hidden() {
        // Folded into the run above; contributes nothing but is still counted
        // by the head's summary. A block folded away with its turn's process
        // section contributes nothing at all.
        return RenderedBlock::default();
    }

    let content_width = chrome::content_width(width)
        .saturating_sub(group_indent(block))
        .max(8);
    let label = kind_label(block.kind, strings);
    let dense = is_dense_row(block.kind);
    // A running thought the reader has not opened, at the end of the session:
    // the one block whose body is drawn without an explicit expansion, inside a
    // window on its tail.
    let live = block.reasoning_window && body_open && next.is_none();
    // `Block::is_open` means "not collapsible" as well as "the reader opened
    // it"; a dense row is showing one line because that is its shape, so only
    // an explicit expansion reveals its body.
    let open = if dense { body_open } else { block.is_open() };
    // A work row's words are the record of what happened, not the text the
    // reader came for: they sit one grey under the muted step, so a run of tool
    // calls recedes behind the answer it produced. Failure stays loud, and a
    // running row's own mark — its lit bullet and the turn line below — already
    // says "now", so its words are quiet too.
    let title_style = match block.kind {
        TimelineRowKind::UserMessage => theme.base().add_modifier(Modifier::BOLD),
        _ if block.activity.is_some() && block.failed => theme.danger(),
        _ if block.activity.is_some() => theme.quiet(),
        _ => theme.base(),
    };
    let mut parts: Vec<(String, Style)> = Vec::new();
    if let Some((glyph, style)) = bullet(block, live, theme) {
        parts.push((format!("{glyph} "), style));
    }
    if block.activity.is_none() && shows_kind_label(block.kind, &block.title, label) {
        parts.push((
            format!("{label} "),
            theme.dimmed(theme.roles.gray).add_modifier(Modifier::BOLD),
        ));
    }
    // The row that stands for a run of work is a summary of it, not the work:
    // it wears the same quiet grey as the rows it folds.
    let (heading, title_style) = if let Some(summary) = &block.group_summary {
        (summary.label(strings.locale), theme.quiet())
    } else if let Some(activity) = &block.activity {
        (activity.action().to_string(), title_style)
    // A notice's title is its severity and its body is the sentence, so the
    // sentence is what the row shows — with the severity carried by colour
    // rather than by a word the reader has to skip.
    } else if matches!(block.kind, TimelineRowKind::SystemNotice) {
        let style = match block.title.trim() {
            "Warning" => theme.warning(),
            "Error" => theme.danger(),
            _ => theme.dimmed(theme.roles.gray),
        };
        (
            dense_summary(block).unwrap_or_else(|| block.title.clone()),
            style,
        )
    } else {
        (
            if block.kind == TimelineRowKind::Reasoning {
                if live {
                    // The ellipsis is the running state: the row it sits on is
                    // the head of a window, not a finished thought.
                    format!("{label}…")
                } else {
                    label.to_string()
                }
            } else {
                block.title.clone()
            },
            title_style,
        )
    };
    parts.push((heading, title_style));
    // The one detail that identifies a dense row, dimmed beside its title.
    // Its shape is independent of whether the work is still running.
    if let GroupRole::Head { members } = block.group
        && members > 1
        && block.group_summary.is_none()
    {
        // Before the summary: a truncated row must still say how many rows it
        // stands for.
        parts.push((
            format!("  +{}", members - 1),
            theme.dimmed(theme.roles.gray_dim),
        ));
    }
    if dense
        && !block.group.is_head()
        && (!open || block.activity.is_some())
        && block.group_summary.is_none()
        && !matches!(block.kind, TimelineRowKind::SystemNotice)
        && let Some(summary) = dense_summary(block)
    {
        parts.push((
            format!(
                "  {}",
                summary.split_whitespace().collect::<Vec<_>>().join(" ")
            ),
            if block.activity.is_some() {
                title_style
            } else {
                theme.muted()
            },
        ));
    }
    if dense && block.failed && block.activity.is_none() {
        parts.push((format!("  {}", strings.failed()), theme.danger()));
    }
    if block.collapsible && !block.expanded && !dense {
        parts.push((
            format!("  ({})", strings.transcript_collapsed_hint()),
            theme.dimmed(theme.roles.gray_dim),
        ));
    }
    let detail_body =
        (!block.details.is_empty() && open).then(|| tool_detail::text(&block.details));
    let body = detail_body
        .as_deref()
        .unwrap_or(&block.body)
        .trim_end_matches('\n');
    // A message *is* its text: a "You" or "Agent" row above every one of them
    // spends a row of the transcript saying what the rail colour already says.
    // The reader's own message keeps a prompt mark on its first line so the two
    // speakers stay distinguishable in a long scroll.
    let headerless = matches!(
        block.kind,
        TimelineRowKind::UserMessage | TimelineRowKind::AgentMessage
    ) && !body.is_empty();
    let prompt_mark = if headerless && matches!(block.kind, TimelineRowKind::UserMessage) {
        Some((prompt_glyph(theme), theme.roles.accent_user))
    } else {
        None
    };
    // The mark costs two columns and the clock whatever its label needs, so the
    // text is wrapped that much narrower. The same width is what the streaming
    // renderer froze its prefix at, which is why it is asked for by one
    // function.
    let body_width = body_width_for(block, width, strings);

    // A dense row is one row: its body is what the fold is for, and the detail
    // overlay can show all of it. Messages render every arriving line; work
    // records keep the same shape until the reader expands them — except a
    // running thought, whose whole reason to be on screen is that the reader
    // watches it arrive.
    let shows_body = !block.group.is_head() && !body.is_empty() && (!dense || open || live);
    // Built before the header: a live window's rail is one cell per row of the
    // window, and the header row carries its first cell.
    let mut rendered = shows_body.then(|| {
        let prose = if block.kind == TimelineRowKind::UserMessage {
            theme.base()
        } else {
            theme.prose()
        };
        let mut rendered = match streamed {
            Some(rendered) => rendered.clone(),
            None if is_markdown(block.kind) => {
                crate::markdown::render_markdown_with(body, theme, body_width, strings, prose)
            }
            None if block.kind == TimelineRowKind::UserMessage => {
                let plain = crate::text::wrap_source_text(body, body_width)
                    .into_iter()
                    .map(|row| row.text)
                    .collect::<Vec<_>>();
                let lines = plain
                    .iter()
                    .map(|text| {
                        Line::from(Span::styled(
                            text.clone(),
                            theme.base().bg(theme.roles.surface_raised),
                        ))
                    })
                    .collect();
                crate::markdown::RenderedMarkdown { lines, plain }
            }
            None => render_plain(body, theme, body_width),
        };
        // Markdown separates paragraphs with a blank row, including the last
        // one; the block gap is the separator between blocks, and two of them
        // is one row of a session spent on nothing.
        while rendered
            .plain
            .last()
            .is_some_and(|line| line.trim().is_empty())
        {
            rendered.lines.pop();
            rendered.plain.pop();
        }
        // Fold before wrapping: a collapsed block must not pay for the lines it
        // is not going to show.
        if !open && !block.streaming && rendered.height() > COLLAPSED_BODY_LINES {
            rendered.lines.truncate(COLLAPSED_BODY_LINES);
            rendered.plain.truncate(COLLAPSED_BODY_LINES);
        }
        rendered
    });
    // A running thought is held to a fixed window on its tail: the newest rows
    // are the ones being read, and once the window is full the oldest leaves
    // the top instead of the block growing without bound. Every row of the
    // window is content, so the rail is one cell per row of the window, its
    // header included, and the bar the reader sees is exactly as tall as the
    // range it stands for.
    if let Some(rendered) = rendered.as_mut()
        && live
    {
        fold_live_window(rendered);
    }

    let fold_title = block.fold.as_ref().filter(|fold| fold.draws_title());
    let fold_control = block.fold.as_ref().filter(|fold| fold.is_control());
    // The cursor is the reader's place, and a block's place is its first drawn
    // row. A fold draws its own rows before the block's, so when one of them is
    // there *it* is the block's first row: the author who pressed the title
    // sees the title lit, not the answer's opening line under it.
    let furniture_holds_cursor = fold_title.is_some() || fold_control.is_some();
    // The rows the fold spent before the block's own content, so the group
    // rail and the connector column are drawn on the rows that belong to the
    // run and not on the turn's title above it.
    let furniture_rows = fold_title_rows(fold_title.is_some()) + fold_divider_rows(block);
    let mut mark = None::<RowMark>;

    // The title above an open process section, on the turn's first process row:
    // the desktop's own header, so a reader who opened the work still knows
    // whose turn it is and can put it away again from where they entered.
    if let Some(fold) = fold_title {
        let (line, text, column) = fold_title_line(
            crate::glyphs::disclosure(true, crate::glyphs::GlyphTier::of(theme)),
            &format!("{} {}", strings.transcript_worked_for(), fold.label),
            content_width,
            theme,
            selected,
            fold.drawn_identity(pictures).map(str::to_owned),
            phase,
        );
        if let Some((column, identity)) = column {
            mark = Some(RowMark {
                line: lines.len(),
                column,
                identity,
            });
        }
        lines.push(line);
        plain.push(text);
        // The air under it: a duration with the first tool row against it reads
        // as that row's heading rather than as the section's.
        lines.push(row_line(Span::raw(" "), Vec::new()));
        plain.push(String::new());
    }

    if !headerless {
        let header = truncate_parts(parts, content_width);
        let header_plain = header
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let pointer = selected && !furniture_holds_cursor;
        let mut header_line = row_line(row_marker(0, live, pointer, phase, theme), header);
        if pointer {
            header_line = header_line.style(Style::default().bg(theme.roles.surface_highlight));
        }
        lines.push(header_line);
        plain.push(header_plain);
    }

    // The turn's fold control and the rule that closes its process section, on
    // the first row of the answer: everything above is work, everything below
    // is what the work was for. It is drawn before the body so a long answer
    // never buries the gesture that hides the work it came from. While the
    // section is shut it is the turn's title instead, since there is no work
    // above it to separate the answer from.
    if let Some(fold) = fold_control {
        // The rule carries the cursor only when there is no title above it to
        // carry it first: the two rows are one gesture, and one gesture has one
        // place.
        let pointer = selected && fold_title.is_none();
        if fold.expanded {
            let (rule_line, rule_plain) =
                divider_line(fold, content_width, theme, pointer, strings, phase);
            lines.push(rule_line);
            plain.push(rule_plain);
        } else {
            let (line, text, column) = fold_title_line(
                crate::glyphs::disclosure(false, crate::glyphs::GlyphTier::of(theme)),
                &format!("{} {}", strings.transcript_worked_for(), fold.label),
                content_width,
                theme,
                pointer,
                fold.drawn_identity(pictures).map(str::to_owned),
                phase,
            );
            if let Some((column, identity)) = column {
                mark = Some(RowMark {
                    line: lines.len(),
                    column,
                    identity,
                });
            }
            lines.push(line);
            plain.push(text);
        }
        // Either way the answer keeps its own air: the rule is where the work
        // stops, and a sentence drawn against it reads as more of the work.
        lines.push(row_line(Span::raw(" "), Vec::new()));
        plain.push(String::new());
    }

    if let Some(rendered) = rendered {
        let body_background = body_band(block, theme);
        let style = body_style(block, theme);
        // The reader's own message is drawn on a band that fills the row: the
        // band is the message's container, not a highlight behind its words.
        let band = band_fills_row(block).then(|| Style::default().bg(theme.roles.surface_raised));
        // The clock is furniture on the row the message starts on: the reader
        // asking "when was this said" reads the first line, not the last one a
        // long answer happened to reach.
        let clock = clock_label(block, strings);
        let clock_style = match band {
            Some(band) => band.fg(theme.roles.gray_dim),
            None => Style::default().fg(theme.roles.gray_dim),
        };
        // The reader's own message carries the box the composer does: the edge
        // that marks it in the column every block spends on its marker, then the
        // padding cell every block spends on its margin — the same two cells the
        // composer puts in front of its draft. A band that hugged its text would
        // read as a highlight behind a sentence rather than as the thing they
        // wrote into.
        let mut block_row = 0usize;
        // Where the box starts, so the cursor can light the whole of it once the
        // last row has been drawn — the box is one object, and a cursor that
        // arrived row by row would light its top before its bottom existed.
        let box_start = lines.len();
        if band.is_some() {
            for _ in 0..BOX_PAD_ROWS {
                lines.push(band_row(content_width, theme));
                plain.push(String::new());
                block_row += 1;
            }
        }
        for (index, (line, text)) in rendered.lines.into_iter().zip(rendered.plain).enumerate() {
            let mut styled = line;
            if !matches!(block.kind, TimelineRowKind::Error) {
                styled = styled.style(style);
            }
            if let Some(background) = body_background {
                styled = styled.style(background);
            }
            let row_style = styled.style;
            let mut spans = styled.spans;
            let mut text = text;
            if index == 0
                && let Some((glyph, colour)) = prompt_mark
            {
                let mut marked = vec![Span::styled(
                    format!("{glyph} "),
                    Style::default().fg(colour).add_modifier(Modifier::BOLD),
                )];
                marked.append(&mut spans);
                spans = marked;
                text = format!("{glyph} {text}");
            }
            if index > 0 && prompt_mark.is_some() {
                spans.insert(0, Span::raw("  "));
                text = format!("  {text}");
            }
            // Neither the band nor the clock is text, so neither enters
            // `plain`: what a reader drags out of the transcript is the message
            // they wrote, without the chrome that framed it. The box's own edge
            // and padding are that chrome, which is why the text is measured
            // from where it starts rather than from the edge of the band.
            close_body_row(
                &mut spans,
                display_width(&text),
                content_width,
                band,
                (index == 0).then_some(clock.as_deref()).flatten(),
                clock_style,
            );
            // The pointer is the first body row's marker only where there is no
            // rail to give it up and no fold row above to carry it: a live
            // window's rail runs unbroken, and a fold's title or rule is the
            // block's first drawn row. The rail counts from the header, so the
            // body's first row is cell one whatever padding the block drew
            // above it. A boxed block has no pointer cell at all: its margin
            // already carries the edge, and the cursor is spent on the box as a
            // whole instead.
            let pointer =
                selected && band.is_none() && block_row == 0 && !live && !furniture_holds_cursor;
            // The marker: the pointer where the block has a margin to spend, and
            // the reader's own edge where the block is a box.
            let marker = if band.is_some() {
                box_edge(theme)
            } else {
                row_marker(block_row + 1, live, pointer, phase, theme)
            };
            let mut row = row_line(marker, spans).style(row_style);
            if block_row == 0 && pointer {
                row = row.style(Style::default().bg(theme.roles.surface_highlight));
            }
            lines.push(row);
            plain.push(text);
            block_row += 1;
        }
        if band.is_some() {
            for _ in 0..BOX_PAD_ROWS {
                lines.push(band_row(content_width, theme));
                plain.push(String::new());
            }
            // The cursor is on this box: light it, padding and all. It is the
            // reader's own surface, and the gesture that says "you are here" on
            // a thing they wrote is the thing itself going bright rather than a
            // glyph appearing in the column that already means "your message".
            if selected && !furniture_holds_cursor {
                lift_box(&mut lines[box_start..], theme);
            }
        }
    }

    let mut status = Vec::new();
    // An Error block already says it failed; repeating the word on its own row
    // adds a line without adding information.
    if block.failed && !dense && block.kind != TimelineRowKind::Error {
        status.push((strings.failed().to_string(), theme.danger()));
    }
    if block.pending_permission {
        status.push((
            strings.approval_waiting().to_string(),
            theme.warning().add_modifier(Modifier::BOLD),
        ));
    }
    // The reader's own message has no runtime: they wrote it, and naming the
    // Agent under their words says the opposite of what happened.
    let attributed = show_attribution && block.kind != TimelineRowKind::UserMessage && body_open;
    if let Some(runtime) = block.runtime_attribution.as_ref().filter(|_| attributed) {
        status.push((
            format!("[{}]", runtime_attribution_line(strings, runtime)),
            theme.muted(),
        ));
    }
    for (text, style) in status {
        let text = truncate_to_width(&text, content_width, "…");
        lines.push(row_line(
            Span::raw(" "),
            vec![Span::styled(text.clone(), style)],
        ));
        plain.push(text);
    }

    // Trailing gap so blocks are separated without a rule.
    for _ in 0..gap_with_body(block, next, body_open) {
        lines.push(row_line(Span::raw(" "), Vec::new()));
        plain.push(String::new());
    }

    // The outer line joins activity headers; the inner line owns one tool's
    // details. Keep their cells in plain text as spaces so pointer selection
    // stays aligned without copying structural glyphs into the clipboard.
    let gap = gap_with_body(block, next, body_open);
    let content_end = lines.len().saturating_sub(gap);
    let connector = crate::glyphs::connector(crate::glyphs::GlyphTier::of(theme));
    for (index, (line, text)) in lines.iter_mut().zip(&mut plain).enumerate() {
        if index < furniture_rows {
            continue;
        }
        let mut prefix = Vec::new();
        if let GroupRole::Member { last, .. } = block.group {
            let glyph = if !last || index == 0 { connector } else { " " };
            prefix.push(Span::styled(
                format!("{glyph} "),
                theme.dimmed(theme.roles.gray_dim),
            ));
        }
        if index > 0 && detail_indent(block) > 0 && index < content_end {
            prefix.push(Span::styled(
                format!("{connector} "),
                theme.dimmed(theme.roles.gray_dim),
            ));
        }
        if !prefix.is_empty() {
            *text = format!("{}{text}", " ".repeat(prefix.len() * chrome::NEST));
            line.spans.splice(2..2, prefix);
        }
    }

    RenderedBlock {
        height: lines.len(),
        lines,
        plain,
        mark,
    }
}

/// Build one rendered row: the marker column, the pad, then the content.
///
/// The marker is the block's selection pointer, a cell of a live window's rail,
/// or a space. It takes the first of the margin columns rather than a column of
/// its own, so a marked block's text stays on the same column as every other
/// block's — a row that shifted by one would break the left edge the reader
/// scans down.
fn row_line(marker: Span<'static>, spans: Vec<Span<'static>>) -> Line<'static> {
    let marker_width = display_width(marker.content.as_ref());
    debug_assert!(marker_width <= chrome::PAD_LEFT);
    let pad = chrome::PAD_LEFT.saturating_sub(marker_width);
    let mut out = Vec::with_capacity(spans.len() + 2);
    out.push(marker);
    out.push(Span::raw(" ".repeat(pad)));
    out.extend(spans);
    Line::from(out)
}

/// The row that names a turn's process section: the mark that folds it, the
/// Agent's brand mark when the terminal can hold one, and the duration.
///
/// It is drawn at the top of an open section and *is* the control while the
/// section is shut — the desktop's "worked for" header, in both places. The
/// gesture is the disclosure at the left and the words beside it; the picture
/// sits in a slot of its own, so the label starts in the same column whether or
/// not the terminal could draw the Agent.
fn fold_title_line(
    glyph: &str,
    label: &str,
    content_width: usize,
    theme: &TuiTheme,
    selected: bool,
    identity: Option<String>,
    phase: u32,
) -> (Line<'static>, String, Option<(u16, String)>) {
    let glyph_width = display_width(glyph);
    // One cell separates the disclosure from what follows; the mark's slot is
    // two cells, the width a picture is drawn in; and the label is framed by one
    // space on either side. At a width too narrow for all of it the words are
    // what matter, so the label gives way rather than the row overflowing.
    let slot = identity
        .as_ref()
        .map_or(0, |_| usize::from(crate::row_marks::MARK_COLS));
    let budget = content_width
        .saturating_sub(glyph_width + 1 + slot + 2)
        .max(1);
    let label = truncate_to_width(label, budget, "…");
    let mut spans = vec![
        Span::styled(glyph.to_string(), theme.muted()),
        Span::raw(" "),
    ];
    if identity.is_some() {
        spans.push(Span::raw(" ".repeat(slot)));
    }
    spans.push(Span::styled(
        format!(" {label} "),
        theme.dimmed(theme.roles.gray),
    ));
    let text = format!("{glyph} {}{label}", " ".repeat(slot));
    let mut line = row_line(row_marker(0, false, selected, phase, theme), spans);
    if selected {
        line = line.style(Style::default().bg(theme.roles.surface_highlight));
    }
    let mark = identity.map(|identity| ((chrome::PAD_LEFT + glyph_width + 1) as u16, identity));
    (line, text, mark)
}

/// The rule that closes a turn's process section: the control that folds the
/// work at its left, and the turn's duration on the rule itself.
///
/// The rule is where the work stops and the answer begins, so it repeats what
/// the title above the work says. A reader who has scrolled into the middle of
/// a long section meets the duration on the row that ends it rather than having
/// to climb back to the top — and the row is the same one they press to put the
/// work away, so its mark points up into what it closes. A disclosure pointing
/// down would promise an opening that pressing it does not make.
fn divider_line(
    fold: &TurnFold,
    content_width: usize,
    theme: &TuiTheme,
    selected: bool,
    strings: Strings,
    phase: u32,
) -> (Line<'static>, String) {
    let tier = crate::glyphs::GlyphTier::of(theme);
    let glyph = crate::glyphs::disclosure_up(tier);
    // One cell separates the disclosure from the left rule, and two cells frame
    // the label — one space on either side. At a width too narrow for all of it
    // the mark and the start of the words are what matter, so the label gives
    // way rather than the row overflowing the band.
    let mark_width = display_width(glyph) + 1;
    let budget = content_width.saturating_sub(mark_width + 2);
    let label = truncate_to_width(
        &format!("{} {}", strings.transcript_worked_for(), fold.label),
        budget,
        "…",
    );
    let framed = display_width(&label) + 2;
    let rules = content_width.saturating_sub(framed + mark_width);
    let (left, right) = (rules / 2, rules - rules / 2);
    let rule = crate::glyphs::heading_rule(tier);
    let spans = vec![
        Span::styled(glyph.to_string(), theme.muted()),
        Span::raw(" "),
        Span::styled(rule.repeat(left), theme.dimmed(theme.roles.gray_dim)),
        Span::styled(format!(" {label} "), theme.dimmed(theme.roles.gray)),
        Span::styled(rule.repeat(right), theme.dimmed(theme.roles.gray_dim)),
    ];
    let text = format!("{glyph} {}{label}{}", rule.repeat(left), rule.repeat(right));
    let mut line = row_line(row_marker(0, false, selected, phase, theme), spans);
    if selected {
        line = line.style(Style::default().bg(theme.roles.surface_highlight));
    }
    (line, text)
}

/// The marker cell of one row of a block, `row` counting from the header.
///
/// When the block is a live thought window the cell is one step of the rail that
/// marks the window's extent, lit by [`rail_weight`] so a crest travels down it
/// while the thought arrives. A terminal that cannot blend colours draws the
/// rail flat: the bar still marks the window, it simply holds still.
fn row_marker(
    row: usize,
    live: bool,
    pointer: bool,
    phase: u32,
    theme: &TuiTheme,
) -> Span<'static> {
    if !live {
        return Span::raw(if pointer { pointer_glyph(theme) } else { " " }.to_string());
    }
    let colour = theme.fade(theme.roles.accent_thinking, rail_weight(row, phase));
    let glyph = if pointer {
        pointer_glyph(theme)
    } else {
        crate::glyphs::accent_bar(crate::glyphs::GlyphTier::of(theme))
    };
    Span::styled(glyph.to_string(), Style::default().fg(colour))
}

/// How brightly one cell of a live window's rail burns.
///
/// A sine squared travelling down the bar: one crest of full colour with a long
/// dim tail, which the eye reads as a single highlight moving rather than as a
/// bar blinking on and off.
fn rail_weight(row: usize, phase: u32) -> f32 {
    use std::f32::consts::PI;
    // The phase grows, the row term is subtracted, so the crest travels *down*
    // the window — the direction the thought itself is moving.
    let angle = phase as f32 * RAIL_SPEED - (row as f32 / RAIL_WAVE_ROWS) * 2.0 * PI;
    let wave = angle.sin();
    // Never fully dark: the rail's job is to mark the window's extent even at
    // the trough of the wave, and a bar that vanished there would read as a
    // rendering fault rather than as motion.
    0.3 + 0.7 * wave * wave
}

/// Hold a live thought to its window.
///
/// The newest rows are kept and the oldest leave the top, which is what makes
/// the window fixed: a thought a hundred lines long costs the same rows on
/// screen as a thought ten lines long. Nothing is spent saying that rows have
/// left: the row is only ever windowed while it is still arriving, so its live
/// header and the rail down its side already say the thought is unfinished, and
/// a mark of its own would take a row from the newest text the window is for.
fn fold_live_window(rendered: &mut crate::markdown::RenderedMarkdown) {
    let window = STREAMING_WINDOW_LINES.max(1);
    if rendered.lines.len() > window {
        let cut = rendered.lines.len() - window;
        rendered.lines.drain(..cut);
        rendered.plain.drain(..cut);
    }
}

/// Close a body row: fill its band to the content edge and, on the row a
/// message starts on, place the clock at the right.
///
/// `used` is the width the row's text already occupies. The body was wrapped to
/// leave the clock's columns free, so the clock lands in the margin rather than
/// on the words; a row whose text overran them anyway — a markdown table is
/// wider than the prose around it — closes its band and goes without a stamp.
fn close_body_row(
    spans: &mut Vec<Span<'static>>,
    used: usize,
    content_width: usize,
    band: Option<Style>,
    clock: Option<&str>,
    clock_style: Style,
) {
    let remaining = content_width.saturating_sub(used);
    // Filling is what turns a band into a block; a row without one only ever
    // gets the empty cells that hold the clock off its text, and those are left
    // unstyled so the page shows through.
    let fill = |spans: &mut Vec<Span<'static>>, width: usize| {
        if let Some(band) = band
            && width > 0
        {
            spans.push(Span::styled(" ".repeat(width), band));
        }
    };
    let Some(clock) = clock else {
        return fill(spans, remaining);
    };
    let clock_width = display_width(clock);
    if remaining < clock_width + CLOCK_EDGE {
        return fill(spans, remaining);
    }
    // Everything the text did not take, less the clock and the cell kept clear
    // between it and the edge of the row.
    let pad = remaining - clock_width - CLOCK_EDGE;
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), band.unwrap_or_default()));
    }
    spans.push(Span::styled(
        format!("{clock}{}", " ".repeat(CLOCK_EDGE)),
        clock_style,
    ));
}

/// The background band a block body sits on, when it benefits from one.
fn body_band(block: &Block, theme: &TuiTheme) -> Option<Style> {
    if block.kind == TimelineRowKind::UserMessage {
        return Some(Style::default().bg(theme.roles.surface_raised));
    }
    if !block.is_open() {
        return None;
    }
    match block.kind {
        // Tool output is quoted material: a band says "this is the machine
        // talking" and separates it from prose without a border.
        TimelineRowKind::Command
        | TimelineRowKind::ToolCall
        | TimelineRowKind::FileOperation
        | TimelineRowKind::WebSearch => Some(Style::default().bg(theme.roles.surface)),
        TimelineRowKind::Error => Some(Style::default().bg(theme.roles.surface)),
        _ => None,
    }
}

fn body_style(block: &Block, theme: &TuiTheme) -> Style {
    match block.kind {
        TimelineRowKind::Reasoning => Style::default()
            .fg(theme.roles.gray)
            .add_modifier(Modifier::ITALIC),
        TimelineRowKind::Command | TimelineRowKind::ToolCall | TimelineRowKind::FileOperation => {
            theme.base()
        }
        TimelineRowKind::Error => theme.danger(),
        TimelineRowKind::SystemNotice | TimelineRowKind::GitNotice => {
            Style::default().fg(theme.roles.gray)
        }
        TimelineRowKind::UserMessage => theme.base(),
        _ => Style::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale::Locale;
    use crate::theme::{ColorCapability, ColorMode, GlyphMode, TuiTheme};
    use vibex_ui::GpuiThemeMode;

    fn theme() -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-dark"),
            GpuiThemeMode::Dark,
            ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Unicode,
            },
        )
    }

    fn strings() -> Strings {
        Strings::for_locale(Locale::En)
    }

    fn block(id: &str, kind: TimelineRowKind, body: &str) -> Block {
        Block {
            id: id.to_string(),
            kind,
            title: if is_work_item(kind) {
                "work".into()
            } else {
                format!("{id} title")
            },
            body: body.to_string(),
            turn_id: Some("turn-1".into()),
            sequence: 1,
            timestamp_ms: None,
            expanded: false,
            reasoning_window: false,
            collapsible: true,
            streaming: false,
            failed: false,
            pending_permission: false,
            file_path: None,
            runtime_attribution: None,
            conclusion: false,
            group: GroupRole::Solo,
            fold: None,
            activity: None,
            details: Vec::new(),
            group_summary: None,
        }
    }

    fn transcript_with(count: usize, body: &str) -> Transcript {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(
            (0..count)
                .map(|index| block(&format!("b{index}"), TimelineRowKind::AgentMessage, body))
                .collect(),
        );
        transcript
    }

    #[test]
    fn a_scrolled_past_prompt_pins_above_the_viewport() {
        let mut transcript = Transcript::new();
        transcript.configure(40, &theme());
        let mut blocks = Vec::new();
        for index in 0..6 {
            blocks.push(block(
                &format!("prompt-{index}"),
                TimelineRowKind::UserMessage,
                "a question the reader asked",
            ));
            blocks.push(block(
                &format!("reply-{index}"),
                TimelineRowKind::AgentMessage,
                "an answer that is long enough to push the question off screen",
            ));
        }
        transcript.set_blocks(blocks);
        let palette = theme();
        let strings = strings();
        // Following the tail of a long transcript still pins the current prompt.
        let header = transcript
            .sticky_header(ScrollState::default(), 12, &palette, strings)
            .expect("the last prompt has scrolled away");
        assert!(
            header.lines.len() <= MAX_STICKY_ROWS,
            "a pinned header must stay short: {}",
            header.lines.len()
        );
        // The header draws the reader's own message, not an Agent reply.
        assert_eq!(
            transcript.blocks()[header.block].kind,
            TimelineRowKind::UserMessage
        );

        // At the very top nothing is pinned: the prompt is already visible.
        let top = transcript.sticky_header(
            ScrollState {
                offset: 0,
                follow: false,
                selected: None,
            },
            12,
            &palette,
            strings,
        );
        assert!(top.is_none());
    }

    #[test]
    fn an_expanded_prompt_does_not_pin() {
        let mut transcript = Transcript::new();
        transcript.configure(40, &theme());
        transcript.set_blocks(vec![
            block("prompt", TimelineRowKind::UserMessage, "a question"),
            block("reply", TimelineRowKind::AgentMessage, "an answer"),
        ]);
        transcript.toggle_block(0);
        let palette = theme();
        let header = transcript.sticky_header(
            ScrollState {
                offset: 3,
                follow: false,
                selected: None,
            },
            10,
            &palette,
            strings(),
        );
        assert!(
            header.is_none(),
            "an expanded prompt is already fully visible"
        );
    }

    #[test]
    fn a_pinned_prompt_is_never_drawn_twice() {
        let mut transcript = Transcript::new();
        transcript.configure(40, &theme());
        let mut blocks = vec![block(
            "prompt",
            TimelineRowKind::UserMessage,
            "a question long enough to wrap over several rows of its own band and keep going",
        )];
        for index in 0..8 {
            blocks.push(block(
                &format!("reply-{index}"),
                TimelineRowKind::AgentMessage,
                "an answer that is long enough to push the question off screen",
            ));
        }
        transcript.set_blocks(blocks);
        let palette = theme();
        let strings = strings();
        let band = 12;
        // The lowest line a window of this height can start on: the header must
        // stay above it however far the reader scrolls, because every row from
        // there down is one the transcript below draws itself.
        let window_floor = transcript.bottom_offset(band, &palette, strings);
        assert!(window_floor > 0, "the fixture must be taller than its band");
        let mut pinned = 0usize;
        for offset in 0..window_floor + band {
            let header = transcript.sticky_header(
                ScrollState {
                    offset,
                    follow: false,
                    selected: None,
                },
                band,
                &palette,
                strings,
            );
            let Some(header) = header else { continue };
            pinned += 1;
            let start = transcript.line_of_block(header.block);
            let window_top = offset.min(window_floor);
            assert!(
                start + header.lines.len() <= window_top,
                "at offset {offset} the pinned prompt reaches row {} of a window that starts at {window_top}",
                start + header.lines.len()
            );
        }
        assert!(pinned > 0, "the fixture must pin its prompt somewhere");

        // The same rule while the reader follows the tail: the body is anchored
        // to the end, so the header holds the rows that end has left behind.
        let header = transcript
            .sticky_header(ScrollState::default(), band, &palette, strings)
            .expect("the prompt has scrolled away from the tail");
        let start = transcript.line_of_block(header.block);
        assert!(start + header.lines.len() <= window_floor);
    }

    #[test]
    fn a_search_highlight_splits_a_line_without_changing_its_text() {
        let pattern = crate::search::SearchPattern::compile("upload").expect("compiles");
        let line = Line::from(vec![
            Span::styled("fix the ".to_string(), Style::default()),
            Span::styled(
                "upload".to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(" path".to_string(), Style::default()),
        ]);
        let text = |line: &Line<'static>| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let before = text(&line);
        let highlighted = highlight_line(
            line,
            &pattern,
            Style::default().add_modifier(Modifier::REVERSED),
        );
        assert_eq!(text(&highlighted), before);
        let matched = highlighted
            .spans
            .iter()
            .find(|span| span.content == "upload")
            .expect("the match survives as its own span");
        assert!(matched.style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn a_column_paint_keeps_every_grapheme_and_only_changes_style() {
        let line = Line::from(vec![
            Span::styled("ab".to_string(), Style::default()),
            Span::styled("中文cd".to_string(), Style::default()),
        ]);
        let highlight = Style::default().add_modifier(Modifier::REVERSED);
        let text = |line: &Line<'static>| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let painted_text = |line: &Line<'static>| {
            line.spans
                .iter()
                .filter(|span| span.style.add_modifier.contains(Modifier::REVERSED))
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        // Columns 2..4 are the two cells of the first wide glyph.
        let painted = paint_columns(line.clone(), 2, 4, highlight);
        assert_eq!(text(&painted), text(&line));
        assert_eq!(painted_text(&painted), "中");
        // Columns 2..6 cover both wide glyphs.
        assert_eq!(
            painted_text(&paint_columns(line.clone(), 2, 6, highlight)),
            "中文"
        );
        // A boundary that falls inside a wide glyph paints the whole glyph,
        // because half a cell is not something a terminal can draw.
        assert_eq!(painted_text(&paint_columns(line, 2, 3, highlight)), "中");
    }

    #[test]
    fn a_word_selection_covers_the_whole_path() {
        let text = "read src/net/upload.rs now";
        // The click lands inside `net`.
        let (start, end) = word_at(text, 8).expect("a word under the pointer");
        assert_eq!(
            &text[usize::from(start)..usize::from(end)],
            "src/net/upload.rs"
        );
        // Whitespace selects nothing.
        assert!(word_at(text, 4).is_none());
        // A column past the end of the line is not a word either.
        assert!(word_at(text, 200).is_none());
    }

    #[test]
    fn plain_lines_reach_past_the_viewport() {
        let mut transcript = transcript_with(40, "a line of body text");
        let palette = theme();
        let lines = transcript.plain_lines(0, 40, &palette, strings());
        assert!(!lines.is_empty());
        assert!(
            lines.len() >= 40,
            "a range is not a viewport: {}",
            lines.len()
        );
    }

    #[test]
    fn appending_a_block_does_not_invalidate_the_others() {
        let mut transcript = transcript_with(3, "hello");
        let strings = strings();
        let palette = theme();
        // Warm the cache.
        transcript.visible_lines(
            ScrollState {
                offset: 0,
                follow: false,
                selected: None,
            },
            200,
            &palette,
            strings,
        );
        let measured_before = transcript.stats.blocks_measured;

        let mut blocks = transcript.blocks().to_vec();
        blocks.push(block("b3", TimelineRowKind::AgentMessage, "new"));
        let change = transcript.set_blocks(blocks);
        assert_eq!(change.appended, 1);
        assert_eq!(change.replaced, 0);

        transcript.visible_lines(
            ScrollState {
                offset: 0,
                follow: true,
                selected: None,
            },
            200,
            &palette,
            strings,
        );
        // Only the new block needed measuring.
        assert_eq!(transcript.stats.blocks_measured, measured_before + 1);
    }

    #[test]
    fn streaming_an_existing_block_marks_exactly_one_dirty() {
        let mut transcript = transcript_with(4, "hello");
        let strings = strings();
        let palette = theme();
        transcript.visible_lines(ScrollState::default(), 200, &palette, strings);

        let mut blocks = transcript.blocks().to_vec();
        blocks[3].body.push_str(" more");
        let change = transcript.set_blocks(blocks);
        assert_eq!(change.replaced, 1);
        assert_eq!(change.appended, 0);
        // Only the streaming block loses its measurement; the rest keep theirs.
        assert_eq!(transcript.heights[3], UNMEASURED);
        assert_ne!(transcript.heights[0], UNMEASURED);
    }

    #[test]
    fn identical_input_reports_no_change() {
        let mut transcript = transcript_with(2, "hello");
        let snapshot = transcript.blocks().to_vec();
        let change = transcript.set_blocks(snapshot.clone());
        assert!(!change.any);
        let change = transcript.set_blocks(snapshot);
        assert!(!change.any);
    }

    #[test]
    fn a_prepend_at_the_budget_keeps_the_blocks_that_were_fetched() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let window = (0..MAX_BLOCKS)
            .map(|index| block(&format!("b{index}"), TimelineRowKind::AgentMessage, "body"))
            .collect::<Vec<_>>();
        assert_eq!(transcript.set_blocks(window).appended, MAX_BLOCKS);

        // One older block arrives and pushes the transcript over budget. The
        // trim must come off the newest end: the older block is exactly what
        // the reader just asked for.
        let mut prepended = vec![block("older", TimelineRowKind::AgentMessage, "body")];
        prepended.extend(transcript.blocks().to_vec());
        let change = transcript.set_blocks(prepended);
        assert_eq!(change.removed, 1);
        assert_eq!(transcript.len(), MAX_BLOCKS);
        assert_eq!(transcript.block(0).unwrap().id, "older");
        assert_eq!(
            transcript.block(MAX_BLOCKS - 1).unwrap().id,
            format!("b{}", MAX_BLOCKS - 2)
        );

        // An append over budget keeps the historic behaviour: newest wins.
        let mut appended = transcript.blocks().to_vec();
        appended.push(block("newest", TimelineRowKind::AgentMessage, "body"));
        transcript.set_blocks(appended);
        assert_eq!(transcript.len(), MAX_BLOCKS);
        assert_eq!(transcript.block(0).unwrap().id, "b0");
        assert_eq!(transcript.block(MAX_BLOCKS - 1).unwrap().id, "newest");
    }

    #[test]
    fn a_group_summary_does_not_hide_a_prepend_at_the_history_budget() {
        let mut transcript = Transcript::new();
        let window = (0..MAX_BLOCKS)
            .map(|index| {
                let mut entry = block(&format!("b{index}"), TimelineRowKind::AgentMessage, "body");
                if index < MIN_GROUP_RUN {
                    entry.kind = TimelineRowKind::ToolCall;
                    entry.title = "Read".into();
                    entry.collapsible = true;
                }
                entry
            })
            .collect();
        transcript.set_blocks(window);
        assert!(transcript.blocks()[0].group.is_head());
        let mut prepended = vec![block(
            "older",
            TimelineRowKind::AgentMessage,
            "older content",
        )];
        prepended.extend(transcript.blocks().to_vec());
        transcript.set_blocks(prepended);
        assert_eq!(transcript.blocks()[0].id, "older");
        assert_eq!(
            transcript.blocks().last().unwrap().id,
            format!("b{}", MAX_BLOCKS - 2)
        );
        assert_eq!(
            transcript
                .blocks()
                .iter()
                .filter(|block| !block.group.is_head())
                .count(),
            MAX_BLOCKS
        );
    }

    #[test]
    fn offscreen_blocks_are_not_rendered() {
        let mut transcript = transcript_with(500, "a fairly long body that will wrap a little");
        let strings = strings();
        let palette = theme();
        transcript.visible_lines(ScrollState::default(), 20, &palette, strings);
        // A 20-line viewport must not have rendered all 500 blocks.
        assert!(
            transcript.stats.blocks_rendered < 60,
            "rendered {} blocks for a 20-line viewport",
            transcript.stats.blocks_rendered
        );
        assert!(transcript.stats.blocks_rendered >= 1);
    }

    #[test]
    fn scrolling_to_the_bottom_measures_only_the_tail() {
        let mut transcript = transcript_with(400, "body");
        let strings = strings();
        let palette = theme();
        transcript.visible_lines(ScrollState::default(), 24, &palette, strings);
        let after_top = transcript.stats.blocks_measured;
        transcript.visible_lines(
            ScrollState {
                offset: 0,
                follow: true,
                selected: None,
            },
            24,
            &palette,
            strings,
        );
        assert!(
            transcript.stats.blocks_measured < after_top + 60,
            "tail scroll measured {} extra blocks",
            transcript.stats.blocks_measured - after_top
        );
    }

    #[test]
    fn follow_mode_tracks_the_end_of_the_transcript() {
        let mut transcript = transcript_with(50, "body");
        let strings = strings();
        let palette = theme();
        let lines = transcript.visible_lines(ScrollState::default(), 10, &palette, strings);
        assert_eq!(lines.len(), 10);
        let total = transcript.total_height();
        assert!(total >= 50);

        let mut blocks = transcript.blocks().to_vec();
        blocks.push(block("extra", TimelineRowKind::AgentMessage, "tail"));
        transcript.set_blocks(blocks);
        let lines = transcript.visible_lines(ScrollState::default(), 10, &palette, strings);
        let last = lines.last().unwrap().to_string();
        assert!(
            last.contains("tail") || lines.iter().any(|line| line.to_string().contains("tail"))
        );
    }

    #[test]
    fn width_change_invalidates_measurements() {
        let mut transcript = transcript_with(3, "some body text that wraps differently by width");
        let strings = strings();
        let palette = theme();
        transcript.visible_lines(ScrollState::default(), 50, &palette, strings);
        let before = transcript.stats.blocks_measured;
        transcript.configure(40, &palette);
        transcript.visible_lines(ScrollState::default(), 50, &palette, strings);
        assert!(transcript.stats.blocks_measured > before);
    }

    #[test]
    fn toggling_expansion_changes_the_height() {
        // A Command body is rendered verbatim, so its line count is exact.
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(vec![block(
            "big",
            TimelineRowKind::Command,
            &"line\n".repeat(30),
        )]);
        let strings = strings();
        let palette = theme();
        let collapsed = {
            transcript.visible_lines(ScrollState::default(), 500, &palette, strings);
            transcript.total_height()
        };
        // A dense row is one row: thirty lines of command output cost the
        // transcript a header, not a screenful.
        assert_eq!(collapsed, 1 + chrome::GAP, "{collapsed}");
        assert!(transcript.toggle_block(0));
        let expanded = {
            transcript.visible_lines(ScrollState::default(), 500, &palette, strings);
            transcript.total_height()
        };
        assert!(
            expanded > collapsed + COLLAPSED_BODY_LINES,
            "expanding did not reveal the body: {expanded} !> {collapsed}"
        );
    }

    #[test]
    fn collapsed_blocks_do_not_materialise_their_whole_body() {
        let long = "word ".repeat(2000);
        let mut collapsed = Transcript::new();
        collapsed.configure(80, &theme());
        collapsed.set_blocks(vec![block("big", TimelineRowKind::Command, &long)]);
        let strings = strings();
        let palette = theme();
        collapsed.visible_lines(ScrollState::default(), 100, &palette, strings);
        let collapsed_height = collapsed.total_height();

        let mut expanded = Transcript::new();
        expanded.configure(80, &theme());
        let mut target = block("big", TimelineRowKind::Command, &long);
        target.expanded = true;
        expanded.set_blocks(vec![target]);
        expanded.visible_lines(ScrollState::default(), 100, &palette, strings);
        assert!(
            expanded.total_height() > collapsed_height * 5,
            "collapsing must actually fold the body"
        );
    }

    #[test]
    fn a_tool_call_that_lands_in_front_of_the_reader_stays_folded() {
        // Tool output is payload, not prose: a settled call must not unfold
        // just because the reader was watching it stream. The compact row is
        // what keeps a long session readable, and the call stays the reader's
        // to open deliberately.
        let long = "word ".repeat(2000);
        let strings = strings();
        let palette = theme();
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let mut streaming = block("tool", TimelineRowKind::ToolCall, &long);
        streaming.streaming = true;
        transcript.set_blocks(vec![streaming]);
        transcript.visible_lines(ScrollState::default(), 100, &palette, strings);

        // The call settles: the same block comes back with its flag cleared.
        transcript.set_blocks(vec![block("tool", TimelineRowKind::ToolCall, &long)]);
        let index = transcript
            .index_of_block("tool")
            .expect("the block stays in the transcript");
        let block = transcript.block(index).expect("the block");
        assert!(!block.expanded, "a settled tool call stays folded");
        assert!(!block.is_open(), "its payload must not unfold with it");

        // And the reader can still choose to read the whole call.
        assert!(transcript.toggle_block(index));
        assert!(transcript.block(index).expect("the block").is_open());
    }

    #[test]
    fn block_at_line_round_trips_with_line_of_block() {
        let mut transcript = transcript_with(20, "body text");
        for index in 0..20 {
            let line = transcript.line_of_block(index);
            assert_eq!(transcript.block_at_line(line), Some(index));
        }
    }

    #[test]
    fn search_finds_blocks_by_title_and_body() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let mut first = block("a", TimelineRowKind::AgentMessage, "the answer is 42");
        first.title = "Meaning".into();
        transcript.set_blocks(vec![
            first,
            block("b", TimelineRowKind::Command, "cargo test"),
        ]);
        assert_eq!(transcript.search("meaning"), vec![0]);
        assert_eq!(transcript.search("42"), vec![0]);
        assert_eq!(transcript.search("cargo"), vec![1]);
        assert!(transcript.search("").is_empty());
    }

    #[test]
    fn copy_and_metadata_expose_the_block_contents() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let mut entry = block("a", TimelineRowKind::Command, "cargo test");
        entry.file_path = Some("src/lib.rs".into());
        entry.runtime_attribution = Some("codex".into());
        // The copied text carries the block's own title, whatever the row's
        // kind decides that title is.
        let title = entry.title.clone();
        transcript.set_blocks(vec![entry]);
        let text = transcript.block_text(0).unwrap();
        assert!(text.contains(&title), "{text}");
        assert!(text.contains("cargo test"));
        let metadata = transcript.block_metadata(0).unwrap();
        assert!(metadata.contains("kind=command"));
        assert!(metadata.contains("path=src/lib.rs"));
        assert!(metadata.contains("runtime=codex"));
    }

    #[test]
    fn transcript_budget_is_enforced() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let blocks = (0..MAX_BLOCKS + 10)
            .map(|index| block(&format!("b{index}"), TimelineRowKind::AgentMessage, "x"))
            .collect();
        let change = transcript.set_blocks(blocks);
        assert_eq!(transcript.len(), MAX_BLOCKS);
        assert_eq!(change.removed, 10);
    }

    #[test]
    fn failed_and_pending_blocks_are_visibly_marked() {
        let mut entry = block("a", TimelineRowKind::ToolCall, "ran");
        entry.failed = true;
        entry.pending_permission = true;
        let rendered = render_block(&entry, &theme(), 60, strings());
        let text = rendered.text();
        assert!(text.contains("Failed") || text.contains("failed"));
        assert!(text.contains("blocked") || text.contains("approval") || text.contains("Approval"));
    }

    #[test]
    fn a_bodyless_block_is_a_header_plus_its_gap() {
        let mut entry = block("a", TimelineRowKind::SystemNotice, "");
        entry.collapsible = false;
        let rendered = render_block(&entry, &theme(), 60, strings());
        assert_eq!(rendered.height, 1 + chrome::GAP);
    }

    #[test]
    fn the_current_block_is_marked_without_inverting_its_text() {
        let mut entry = block("a", TimelineRowKind::AgentMessage, "body");
        entry.collapsible = false;
        let plain = render_block(&entry, &theme(), 60, strings());
        let marked = render_block_styled(&entry, &theme(), 60, strings(), true, 0);
        // The marker takes the first margin column, and only on the header, so
        // the text of a marked block stays on the column of every other block.
        assert_eq!(plain.lines[0].spans[0].content.as_ref(), " ");
        assert_eq!(marked.lines[0].spans[0].content.as_ref(), "▌");
        assert_eq!(marked.lines[1].spans[0].content.as_ref(), " ");
        assert_eq!(
            plain.lines[0].spans[1].content.as_ref(),
            marked.lines[0].spans[1].content.as_ref(),
            "the marker moved the text"
        );
        // The header is lifted rather than reversed: reverse video on a whole
        // row is the heaviest emphasis a terminal has.
        let header = marked.lines[0].style;
        assert_eq!(header.bg, Some(theme().roles.surface_highlight));
        assert!(!header.add_modifier.contains(Modifier::REVERSED));
        // The rows under it are not lifted, so only the header reads as current.
        assert_eq!(marked.lines[1].style.bg, None);
        assert_eq!(plain.lines[0].style.bg, None);
    }

    /// A thought long enough to overflow its window.
    fn running_thought() -> Block {
        let mut entry = block(
            "thought",
            TimelineRowKind::Reasoning,
            "- read the cache path\n- check how a delta invalidates it\n- decide where the window lives\n- keep the newest rows\n- drop the oldest\n- hold the window's height\n- draw the rail\n- animate the rail",
        );
        entry.streaming = true;
        entry.expanded = true;
        entry.reasoning_window = true;
        entry
    }

    #[test]
    fn a_running_thought_opens_a_window_on_the_tail_of_its_body() {
        let entry = running_thought();
        let rendered = render_block(&entry, &theme(), 60, strings());
        let text = rendered.plain.join("\n");
        // The header says the thought is still arriving, and the bullet points
        // at the rows below it rather than away from them.
        assert!(
            rendered.plain[0].contains("Thinking…"),
            "{}",
            rendered.plain[0]
        );
        assert!(rendered.plain[0].contains('▾'), "{}", rendered.plain[0]);
        // The newest rows are the ones shown and the oldest have left the top;
        // nothing is spent saying so, so the first row under the header is the
        // oldest text the window still holds.
        assert!(text.contains("animate the rail"), "{text}");
        assert!(!text.contains("read the cache path"), "{text}");
        assert!(
            rendered.plain[1].contains("decide where the window lives"),
            "the window does not open on its oldest kept row: {:?}",
            rendered.plain[1]
        );
        assert!(
            rendered.plain[1..].iter().all(|row| !row.contains('…')),
            "a row of the window was spent on a mark: {:?}",
            rendered.plain
        );
        // The window is bounded: header, window rows, gap.
        assert!(
            rendered.height <= 1 + STREAMING_WINDOW_LINES + chrome::GAP,
            "the window grew past its bound: {} rows",
            rendered.height
        );
    }

    #[test]
    fn a_thought_that_has_landed_folds_to_one_row() {
        let mut entry = running_thought();
        entry.streaming = false;
        entry.expanded = false;
        entry.reasoning_window = false;
        let rendered = render_block(&entry, &theme(), 60, strings());
        assert_eq!(rendered.height, 1 + chrome::GAP);
        assert!(
            rendered.plain[0].contains("Thinking"),
            "{:?}",
            rendered.plain
        );
        assert!(!rendered.plain[0].contains('…'), "{:?}", rendered.plain);
        assert!(rendered.plain[0].contains('▸'), "{:?}", rendered.plain);
    }

    #[test]
    fn the_window_rail_spans_the_window_and_moves_with_the_clock() {
        let entry = running_thought();
        let palette = theme();
        let at = |phase| render_block_at_phase(&entry, &palette, 60, strings(), phase);
        let early = at(0);
        let later = at(4);
        let bar = crate::glyphs::accent_bar(crate::glyphs::GlyphTier::Full);
        // One cell per row of the window, the header included, so the bar the
        // reader sees is exactly as tall as the range it stands for.
        for (row, line) in early
            .lines
            .iter()
            .take(1 + STREAMING_WINDOW_LINES)
            .enumerate()
        {
            assert_eq!(line.spans[0].content.as_ref(), bar, "row {row}");
            assert!(line.spans[0].style.fg.is_some(), "row {row} is unlit");
        }
        assert_eq!(
            early.lines[1 + STREAMING_WINDOW_LINES].spans[0]
                .content
                .as_ref(),
            " ",
            "the gap row carries rail"
        );
        // A cell is lit differently a few ticks later, and two cells of one
        // frame differ from each other: the highlight travels rather than the
        // whole bar flashing.
        assert_ne!(
            early.lines[0].spans[0].style.fg,
            later.lines[0].spans[0].style.fg
        );
        assert_ne!(
            early.lines[0].spans[0].style.fg,
            early.lines[1].spans[0].style.fg
        );
        // The cursor takes the header's cell and lights it like the rail cell it
        // replaced; the bar itself carries on below, so the window's height is
        // still what the reader sees.
        let marked = render_block_styled(&entry, &palette, 60, strings(), true, 0);
        assert_eq!(marked.lines[0].spans[0].content.as_ref(), "▌");
        assert_eq!(
            marked.lines[0].spans[0].style.fg,
            early.lines[0].spans[0].style.fg
        );
        assert_eq!(marked.lines[1].spans[0].content.as_ref(), bar);
    }

    #[test]
    fn a_running_thought_is_never_folded_into_a_work_run() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let mut thought = block("live", TimelineRowKind::Reasoning, "weighing the options");
        thought.streaming = true;
        // Three work items in a row would fold into a run. The live thought
        // must not: its window is the thing the reader is watching.
        transcript.set_blocks(vec![
            block("t0", TimelineRowKind::ToolCall, "ran"),
            block("t1", TimelineRowKind::ToolCall, "ran"),
            thought,
        ]);
        assert_eq!(transcript.blocks()[2].group, GroupRole::Solo);
        // Nothing opens itself, so the reader opens the live thought.
        transcript.toggle_block(2);
        let text = transcript
            .visible_lines(ScrollState::default(), 20, &theme(), strings())
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("weighing the options"), "{text}");
    }

    #[test]
    fn the_clock_redraws_the_window_without_laying_it_out_again() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let palette = theme();
        // Nothing opens itself, so the reader's preference opens the window.
        transcript.set_reasoning_preferences(true, ReasoningExpansionMode::Window);
        transcript.set_blocks(vec![running_thought()]);
        let _ = transcript.visible_lines(ScrollState::default(), 30, &palette, strings());
        let first = transcript.stats.blocks_rendered;
        // The same frame twice: the window is resident, so nothing re-renders.
        let _ = transcript.visible_lines(ScrollState::default(), 30, &palette, strings());
        assert_eq!(
            transcript.stats.blocks_rendered, first,
            "an unchanged frame re-rendered the window"
        );
        // A moved clock drops the window's rows — the rail is a frame of the
        // animation — and the height stays measured, so nothing reflows.
        transcript.set_animation_phase(3);
        let _ = transcript.visible_lines(ScrollState::default(), 30, &palette, strings());
        assert!(
            transcript.stats.blocks_rendered > first,
            "the moved clock did not redraw the window"
        );
        assert_eq!(
            transcript.stats.blocks_measured, 1,
            "the clock forced a re-measure"
        );
    }

    #[test]
    fn work_items_carry_a_bullet_and_messages_do_not() {
        let palette = theme();
        let with_bullet = |kind| {
            let mut entry = block("a", kind, "body");
            entry.collapsible = false;
            render_block(&entry, &palette, 60, strings()).plain[0].clone()
        };
        assert!(with_bullet(TimelineRowKind::ToolCall).starts_with('▸'));
        assert!(with_bullet(TimelineRowKind::Reasoning).starts_with('▸'));
        assert!(!with_bullet(TimelineRowKind::AgentMessage).starts_with('▸'));
        assert!(!with_bullet(TimelineRowKind::UserMessage).starts_with('▸'));
    }

    #[test]
    fn a_run_of_collapsed_work_items_folds_into_its_first_member() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(
            (0..6)
                .map(|index| block(&format!("t{index}"), TimelineRowKind::ToolCall, "ran"))
                .collect(),
        );
        assert!(matches!(
            transcript.blocks()[0].group,
            GroupRole::Head { members: 6 }
        ));
        for entry in &transcript.blocks()[1..] {
            assert!(entry.group.is_hidden());
        }
        let rendered = transcript.visible_lines(ScrollState::default(), 60, &theme(), strings());
        let text = rendered
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        // The folded count rides the head's own row rather than spending a row
        // of its own.
        assert!(text.contains("+5"), "{text}");
    }

    #[test]
    fn a_work_run_is_separated_from_the_text_under_it() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let mut blocks = (0..3)
            .map(|index| block(&format!("t{index}"), TimelineRowKind::ToolCall, "ran"))
            .collect::<Vec<_>>();
        blocks.push(block("answer", TimelineRowKind::AgentMessage, "the answer"));
        transcript.set_blocks(blocks);
        let lines = transcript.visible_lines(ScrollState::default(), 60, &theme(), strings());
        let text = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        let summary = text
            .iter()
            .position(|line| line.contains("+2"))
            .expect("the run's summary is drawn");
        let answer = text
            .iter()
            .position(|line| line.contains("the answer"))
            .expect("the answer is drawn");
        // The run is one object among the blocks around it: the sentence that
        // answers it is a paragraph of its own, not the run's last line.
        assert_eq!(
            answer,
            summary + 2,
            "the answer is drawn against the run: {text:?}"
        );
        assert!(
            text[summary + 1].trim().is_empty(),
            "the row under the run is not blank: {:?}",
            text[summary + 1]
        );
    }

    #[test]
    fn an_expanded_or_failed_item_breaks_the_run() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let mut expanded = block("t1", TimelineRowKind::ToolCall, "ran");
        expanded.expanded = true;
        let mut failed = block("t2", TimelineRowKind::ToolCall, "boom");
        failed.failed = true;
        transcript.set_blocks(vec![
            block("t0", TimelineRowKind::ToolCall, "ran"),
            expanded,
            failed,
            block("t3", TimelineRowKind::ToolCall, "ran"),
            block("t4", TimelineRowKind::ToolCall, "ran"),
        ]);
        for entry in transcript.blocks() {
            assert_eq!(
                entry.group,
                GroupRole::Solo,
                "{} should not have been folded",
                entry.id
            );
        }
    }

    #[test]
    fn a_short_run_is_left_alone() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(vec![
            block("t0", TimelineRowKind::ToolCall, "ran"),
            block("t1", TimelineRowKind::ToolCall, "ran"),
        ]);
        for entry in transcript.blocks() {
            assert_eq!(entry.group, GroupRole::Solo);
        }
    }

    #[test]
    fn grouping_keeps_the_transcript_shorter() {
        let body = "output line\n".repeat(12);
        let mut flat = Transcript::new();
        flat.configure(80, &theme());
        let mut entries = (0..8)
            .map(|index| block(&format!("t{index}"), TimelineRowKind::ToolCall, &body))
            .collect::<Vec<_>>();
        for entry in &mut entries {
            entry.expanded = true;
        }
        flat.set_blocks(entries);
        let expanded_height = flat.total_height();

        let mut folded = Transcript::new();
        folded.configure(80, &theme());
        folded.set_blocks(
            (0..8)
                .map(|index| block(&format!("t{index}"), TimelineRowKind::ToolCall, &body))
                .collect(),
        );
        assert!(folded.total_height() * 3 < expanded_height);
    }

    #[test]
    fn a_streaming_block_is_still_what_makes_the_band_live() {
        // The transcript has no animation of its own any more — a delta marks
        // the app dirty — but "something is arriving" is still the question the
        // turn band asks before it draws a spinner.
        let mut transcript = transcript_with(3, "settled");
        assert!(!transcript.is_animating());

        let mut streaming = block("live", TimelineRowKind::AgentMessage, "…");
        streaming.streaming = true;
        let mut entries = transcript.blocks().to_vec();
        entries.push(streaming);
        transcript.set_blocks(entries);
        assert!(transcript.is_animating());
    }

    #[test]
    fn the_width_change_keeps_the_chrome_inside_the_frame() {
        let mut transcript = Transcript::new();
        transcript.configure(40, &theme());
        transcript.set_blocks(vec![block(
            "a",
            TimelineRowKind::AgentMessage,
            "这是一段中文正文，用于验证按列宽换行。",
        )]);
        let palette = theme();
        let lines = transcript.visible_lines(ScrollState::default(), 100, &palette, strings());
        for line in &lines {
            assert!(
                display_width(&line.to_string()) <= 40,
                "a row overflowed the chrome: {line:?}"
            );
        }
    }

    #[test]
    fn a_turn_is_one_the_reader_opened() {
        // The runtime writes rows of its own: a session's startup line lands
        // before anyone has spoken, and the hidden continuation that follows a
        // failed turn gets a turn id of its own from the projection. Neither is
        // a turn the reader took — counting them put a tick on the rail for a
        // conversation that had not started, and one more for every restart.
        let notice = |id: &str, body: &str| {
            let mut entry = block(id, TimelineRowKind::SystemNotice, body);
            entry.turn_id = None;
            entry
        };
        let turn_block = |id: &str, kind: TimelineRowKind, body: &str, turn: &str| {
            let mut entry = block(id, kind, body);
            entry.turn_id = Some(turn.to_string());
            entry
        };
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(vec![
            notice("startup", "Agent session is initializing"),
            turn_block(
                "u1",
                TimelineRowKind::UserMessage,
                "the first question",
                "turn-1",
            ),
            turn_block(
                "a1",
                TimelineRowKind::AgentMessage,
                "the first answer",
                "turn-1",
            ),
            notice("compaction", "Context compacted"),
            turn_block(
                "u2",
                TimelineRowKind::UserMessage,
                "the second question",
                "turn-2",
            ),
            turn_block(
                "a2",
                TimelineRowKind::AgentMessage,
                "the second answer",
                "turn-2",
            ),
            turn_block(
                "continued",
                TimelineRowKind::AgentMessage,
                "the turn the runtime continued on its own",
                "turn:continuation:9",
            ),
        ]);

        assert_eq!(
            transcript.turn_count(),
            2,
            "a row the reader did not write was counted as a turn"
        );
        assert_eq!(
            transcript.block_of_turn(0),
            Some(1),
            "a turn does not start at the message that opened it"
        );
        assert_eq!(transcript.block_of_turn(1), Some(4));
        assert_eq!(transcript.block_of_turn(2), None);

        // The rail's marker follows the same rule. Above the first turn the
        // notice belongs to no turn, so nothing is marked...
        transcript.scroll_offset = 0;
        assert_eq!(transcript.active_turn(), None);
        // ...a notice between turns is the tail of the turn above it, so the
        // marker stays there until the next message reaches the top...
        let between = transcript.line_of_block(3);
        transcript.scroll_offset = between;
        assert_eq!(transcript.active_turn(), Some(0));
        // ...the second turn's message moves it...
        let second = transcript.line_of_block(4);
        transcript.scroll_offset = second;
        assert_eq!(transcript.active_turn(), Some(1));
        // ...and the continuation is more of the turn above it, not a turn.
        let continued = transcript.line_of_block(6);
        transcript.scroll_offset = continued;
        assert_eq!(transcript.active_turn(), Some(1));
    }

    #[test]
    fn the_rail_lights_every_turn_the_viewport_shows() {
        // The reader's place in a session is a window, not a point: a screen
        // showing the end of one answer and the start of the next is in both
        // turns, and a rail that lit one tick under it would say the session
        // was one turn long. The range starts at the anchor the rail has always
        // marked, and ends at the turn the last visible row belongs to.
        let turn = |id: &str, turn: &str| {
            let mut entry = block(id, TimelineRowKind::UserMessage, "a question");
            entry.turn_id = Some(turn.to_string());
            entry
        };
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(vec![
            turn("a", "turn-1"),
            turn("b", "turn-2"),
            turn("c", "turn-3"),
            turn("d", "turn-4"),
        ]);
        let palette = theme();
        // Settle the measurement before the line arithmetic, so the offsets
        // below are measured heights rather than estimates of them.
        let _ = transcript.visible_lines(ScrollState::default(), 200, &palette, strings());

        // A viewport that starts at the top and ends inside the third turn
        // shows all three of them.
        let end = transcript.line_of_block(2);
        let lines = transcript.visible_lines(
            ScrollState {
                follow: false,
                ..Default::default()
            },
            end + 1,
            &palette,
            strings(),
        );
        assert_eq!(lines.len(), end + 1, "the fixture did not fill the band");
        assert_eq!(
            transcript.visible_turns(),
            Some(0..3),
            "a screen of three turns lit something else"
        );

        // One row inside the first turn shows only that turn: the window ends
        // where the viewport does.
        let _ = transcript.visible_lines(
            ScrollState {
                follow: false,
                ..Default::default()
            },
            1,
            &palette,
            strings(),
        );
        assert_eq!(transcript.visible_turns(), Some(0..1));
    }

    #[test]
    fn turns_the_viewport_does_not_show_are_not_lit() {
        let turn = |id: &str, turn: &str| {
            let mut entry = block(id, TimelineRowKind::UserMessage, "a question");
            entry.turn_id = Some(turn.to_string());
            entry
        };
        let mut startup = block("startup", TimelineRowKind::SystemNotice, "initializing");
        startup.turn_id = None;
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(vec![startup, turn("a", "turn-1"), turn("b", "turn-2")]);
        let palette = theme();
        // Settle the measurement before the line arithmetic below.
        let _ = transcript.visible_lines(ScrollState::default(), 500, &palette, strings());

        // The runtime's own startup row is not a turn, so the top of the
        // transcript lights nothing at all...
        let _ = transcript.visible_lines(
            ScrollState {
                follow: false,
                ..Default::default()
            },
            1,
            &palette,
            strings(),
        );
        assert_eq!(transcript.visible_turns(), None);

        // ...and a window that opens above the first turn lights the turns it
        // reaches: the startup row belongs to no turn, so nothing wears the
        // marker, but the screen is showing the conversation all the same.
        let first = transcript.line_of_block(1);
        let second = transcript.line_of_block(2);
        assert!(first > 0, "the fixture has no row above the first turn");
        let _ = transcript.visible_lines(
            ScrollState {
                follow: false,
                ..Default::default()
            },
            second,
            &palette,
            strings(),
        );
        assert_eq!(transcript.visible_turns(), Some(0..1));

        // One row more reaches the second turn, and lights it too.
        let _ = transcript.visible_lines(
            ScrollState {
                follow: false,
                ..Default::default()
            },
            second + 1,
            &palette,
            strings(),
        );
        assert_eq!(transcript.visible_turns(), Some(0..2));
        assert_eq!(
            transcript.active_turn(),
            None,
            "a window that opens above the first turn has no anchor to mark"
        );
    }
}

#[cfg(test)]
mod density_tests {
    use super::*;
    use crate::locale::Locale;

    fn strings() -> Strings {
        Strings::for_locale(Locale::En)
    }

    fn theme() -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-dark"),
            vibex_ui::GpuiThemeMode::Dark,
            crate::theme::ColorCapability {
                mode: crate::theme::ColorMode::TrueColor,
                glyphs: crate::theme::GlyphMode::Unicode,
            },
        )
    }

    #[test]
    fn expanded_groups_survive_updates_and_collapse_back_to_one_row() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let rows = (0..4)
            .map(|index| {
                item(
                    &format!("tool-{index}"),
                    TimelineRowKind::ToolCall,
                    &format!("cargo test package-{index}"),
                    true,
                )
            })
            .collect::<Vec<_>>();
        transcript.set_blocks(rows.clone());
        let collapsed = transcript.visible_lines(ScrollState::default(), 40, &theme(), strings());
        assert!(collapsed.iter().any(|line| line.to_string().contains("+3")));
        let copied = transcript.plain_text();
        assert!(!copied.starts_with('\n'));
        assert_eq!(copied.matches("cargo test package-0").count(), 1);
        assert!(transcript.toggle_block(0));
        assert!(transcript.toggle_block(3));
        let mut updated = rows;
        updated[2].body.push_str("\nall tests passed");
        updated[2].streaming = false;
        transcript.set_blocks(updated);
        let expanded = transcript.visible_lines(ScrollState::default(), 40, &theme(), strings());
        for index in 0..4 {
            assert!(
                expanded
                    .iter()
                    .any(|line| line.to_string().contains(&format!("package-{index}")))
            );
        }
        assert!(
            expanded
                .iter()
                .any(|line| line.to_string().contains("all tests passed"))
        );
        transcript.toggle_all(false);
        let folded = transcript.visible_lines(ScrollState::default(), 40, &theme(), strings());
        assert_eq!(folded.len(), collapsed.len());
        assert_eq!(transcript.total_height(), folded.len());
    }

    #[test]
    fn different_actions_turns_and_failures_cannot_hide_inside_a_group() {
        let mut rows = (0..4)
            .map(|index| {
                item(
                    &format!("t{index}"),
                    TimelineRowKind::ToolCall,
                    "detail",
                    false,
                )
            })
            .collect::<Vec<_>>();
        rows[1].title = "edit".into();
        rows[2].failed = true;
        rows[3].turn_id = Some("next-turn".into());
        let mut transcript = Transcript::new();
        transcript.set_blocks(rows);
        assert!(
            transcript
                .blocks()
                .iter()
                .all(|block| block.group == GroupRole::Solo)
        );
    }

    #[test]
    fn user_text_keeps_literal_syntax_spacing_and_hanging_indent() {
        let row = item(
            "user",
            TimelineRowKind::UserMessage,
            "# heading  **literal**\n第二行文字 abcdefghijklmnopqrstuvwxyz",
            false,
        );
        let rendered = render_block(&row, &theme(), 30, strings());
        // The box pads the text on both sides, so the message starts one row in.
        assert_eq!(rendered.plain[BOX_PAD_ROWS - 1], "");
        assert!(rendered.plain[BOX_PAD_ROWS].contains("# heading  **literal**"));
        assert!(rendered.plain[BOX_PAD_ROWS + 1].starts_with("  第二行"));
        for line in rendered.lines.iter().take(rendered.height - 1) {
            assert_eq!(line.style.bg, Some(theme().roles.surface_raised));
            assert!(line.width() <= 30);
        }
    }

    /// The visible text of one rendered line, spans joined.
    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    /// The clock label a block renders, which is what the assertions look for
    /// in the line: the hour itself belongs to the reader's timezone, and the
    /// placement does not.
    fn clock_of(block: &Block) -> String {
        clock_label(block, strings()).expect("the block carries a clock")
    }

    #[test]
    fn a_message_is_stamped_at_the_right_of_its_first_row() {
        let mut row = item(
            "user",
            TimelineRowKind::UserMessage,
            "what does the retry helper do?",
            false,
        );
        row.timestamp_ms = Some(1_759_237_920_000);
        let width = 60;
        let rendered = render_block(&row, &theme(), width, strings());
        let clock = clock_of(&row);
        // The box pads its text, so the message itself starts below the first
        // row — and the clock rides on the text, not on the padding above it.
        let text_row = BOX_PAD_ROWS;
        let first = line_text(&rendered.lines[text_row]);
        assert!(first.contains(&clock), "no clock on {first:?}");

        // The label ends one cell short of the content's edge, so the row is
        // exactly as wide as every other row of the band.
        assert!(
            first.ends_with(&format!("{clock}{}", " ".repeat(CLOCK_EDGE))),
            "{first:?}"
        );
        let content_width = chrome::content_width(width);
        assert_eq!(
            display_width(&first),
            chrome::PAD_LEFT + content_width,
            "the stamped row is not full width: {first:?}"
        );

        // One clock per message, on the row the message starts at.
        for (index, line) in rendered.lines.iter().enumerate() {
            if index == text_row {
                continue;
            }
            assert!(!line_text(line).contains(&clock), "{:?}", line_text(line));
        }
    }

    #[test]
    fn the_clock_never_lands_on_the_message() {
        // A message long enough to wrap: every line of it has to stop short of
        // the clock, not only the row the clock is drawn on.
        let mut row = item(
            "user",
            TimelineRowKind::UserMessage,
            "The upload helper gives up on the first 5xx. It should retry with a short \
             backoff and give up after three attempts, and the retry has to be covered \
             by a test that fails on the old code.",
            false,
        );
        row.timestamp_ms = Some(1_759_237_920_000);
        let width = 60;
        let rendered = render_block(&row, &theme(), width, strings());
        let clock = clock_of(&row);
        let first = line_text(&rendered.lines[BOX_PAD_ROWS]);
        let at = first.find(&clock).expect("the clock is on the first row");
        let text_before = display_width(&first[..at]);
        let text_width = display_width(&rendered.plain[BOX_PAD_ROWS]);
        assert!(
            text_before >= text_width + CLOCK_GAP,
            "the clock is {text_before} columns in, on text {text_width} wide"
        );
        assert!(rendered.plain.len() > 1, "the message did not wrap");
    }

    #[test]
    fn the_readers_message_is_a_box_that_fills_the_row() {
        let row = item("user", TimelineRowKind::UserMessage, "a question", false);
        let width = 60;
        let content_width = chrome::content_width(width);
        let band = theme().roles.surface_raised;
        let rendered = render_block(&row, &theme(), width, strings());
        for line in rendered.lines.iter().take(rendered.height - 1) {
            let text = line_text(line);
            assert_eq!(
                display_width(&text),
                chrome::PAD_LEFT + content_width,
                "the message band stopped early: {text:?}"
            );
            // The cells past the words are part of the block, not empty page.
            let tail = line.spans.last().expect("the row ends in a span");
            assert_eq!(
                tail.style.bg,
                Some(band),
                "the band stopped at the text: {:?}",
                tail.content
            );
        }
        // The text is padded above and below, the way the composer pads the
        // draft it holds: one line of text is a three-row box, and the padding
        // is not text — it would otherwise be copied out with the message.
        let box_rows = rendered.height - chrome::GAP;
        assert_eq!(box_rows, BOX_PAD_ROWS * 2 + 1, "the box is not padded");
        assert!(rendered.plain[..BOX_PAD_ROWS].iter().all(String::is_empty));
        assert!(
            rendered.plain[box_rows - BOX_PAD_ROWS..box_rows]
                .iter()
                .all(String::is_empty)
        );
        assert!(!rendered.plain[BOX_PAD_ROWS].is_empty());
    }

    /// The cell a line draws at `column`, as the text and the style it wears.
    fn cell_at(line: &Line<'static>, column: usize) -> (String, Style) {
        let mut at = 0usize;
        for span in &line.spans {
            let width = display_width(span.content.as_ref());
            if column < at + width {
                return (span.content.to_string(), span.style);
            }
            at += width;
        }
        (String::new(), Style::default())
    }

    #[test]
    fn the_readers_message_wears_an_edge_down_its_left() {
        let theme = theme();
        let row = item("user", TimelineRowKind::UserMessage, "a question", false);
        let rendered = render_block(&row, &theme, 60, strings());
        let edge = crate::glyphs::accent_bar(crate::glyphs::GlyphTier::of(&theme));
        // The box's own rows, its padding included: the edge is the box, not the
        // text, so a message short enough to fit one row still wears it top to
        // bottom. It is drawn in the column every block spends on its marker —
        // the column the composer draws its own edge in, which is what makes the
        // message and the box it was typed into one surface at two moments.
        let box_rows = rendered.height - chrome::GAP;
        for (index, line) in rendered.lines.iter().take(box_rows).enumerate() {
            let (glyph, style) = cell_at(line, 0);
            assert_eq!(glyph, edge, "row {index} of the box has no edge");
            assert_eq!(
                style.fg,
                Some(theme.roles.accent_success),
                "row {index} of the edge is not the reader's green"
            );
        }
        // The box holds nothing of its own: the padding cell behind the edge is
        // the margin every block spends, so the prompt mark and the words under
        // it start on the composer's own column.
        let body = line_text(&rendered.lines[BOX_PAD_ROWS]);
        let prompt = prompt_glyph(&theme);
        assert!(
            body.starts_with(&format!("{edge} {prompt} a question")),
            "{body:?}"
        );
        // The copy of the box's rows leaves the chrome behind: a reader who
        // drags the message out gets the words, not the rule that framed them.
        assert_eq!(rendered.plain[BOX_PAD_ROWS], format!("{prompt} a question"));

        // A reply is prose on the page, not a surface the reader wrote into.
        let answer = item("agent", TimelineRowKind::AgentMessage, "an answer", false);
        let rendered = render_block(&answer, &theme, 60, strings());
        for line in rendered.lines.iter() {
            assert!(
                !line_text(line).contains(edge),
                "the answer wears the reader's edge"
            );
        }
    }

    #[test]
    fn the_cursor_lights_the_box_without_taking_its_edge() {
        let theme = theme();
        let row = item("user", TimelineRowKind::UserMessage, "a question", false);
        let rendered = render_block_styled(&row, &theme, 60, strings(), true, 0);
        let edge = crate::glyphs::accent_bar(crate::glyphs::GlyphTier::of(&theme));
        let box_rows = rendered.height - chrome::GAP;
        // The edge is drawn on every row of the box, the cursor's included. It
        // used to be swapped for the pointer glyph on the box's first drawn row —
        // a row of padding above the text — where that white cell read as
        // something the reader had put there rather than as a cursor.
        for (index, line) in rendered.lines.iter().take(box_rows).enumerate() {
            let (glyph, style) = cell_at(line, 0);
            assert_eq!(glyph, edge, "row {index} of the selected box lost its edge");
            assert_eq!(
                style.fg,
                Some(theme.roles.accent_success),
                "row {index} of the selected box lost the reader's green"
            );
        }
        // The cursor is spent on the surface instead: every cell behind the edge
        // is lit, padding rows included.
        let lit = Some(theme.roles.surface_highlight);
        for (index, line) in rendered.lines.iter().take(box_rows).enumerate() {
            let tail = line.spans.last().expect("the row ends in a span");
            assert_eq!(tail.style.bg, lit, "row {index} of the box is not lit");
        }
        // The row's own style still says the block is a band: a pinned prompt is
        // clipped by reading exactly that, so the cursor must not change it.
        for (index, line) in rendered.lines.iter().take(box_rows).enumerate() {
            assert_eq!(
                line.style.bg,
                Some(theme.roles.surface_raised),
                "row {index} of the box stopped being a band"
            );
        }
        // Unselected, the box is the raised band it always was.
        let plain = render_block(&row, &theme, 60, strings());
        for (index, line) in plain.lines.iter().take(box_rows).enumerate() {
            let tail = line.spans.last().expect("the row ends in a span");
            assert_eq!(
                tail.style.bg,
                Some(theme.roles.surface_raised),
                "row {index} of an unselected box is lit"
            );
        }
    }

    #[test]
    fn work_rows_and_unstamped_rows_draw_no_clock() {
        // Only the conversation is stamped: a work item's clock would be one of
        // forty labels saying the same minute.
        let mut work = item("tool", TimelineRowKind::ToolCall, "read Cargo.toml", false);
        work.timestamp_ms = Some(1_759_237_920_000);
        assert_eq!(clock_label(&work, strings()), None);
        assert_eq!(clock_columns(&work, strings()), 0);

        // A row the runtime never stamped has nothing to draw and keeps the
        // whole row for its text.
        let unstamped = item("user", TimelineRowKind::UserMessage, "a question", false);
        assert_eq!(unstamped.timestamp_ms, None);
        assert_eq!(clock_label(&unstamped, strings()), None);
        assert_eq!(
            body_width_for(&unstamped, 60, strings()),
            chrome::content_width(60) - crate::glyphs::PROMPT_ARROW_WIDTH
        );
        let rendered = render_block(&unstamped, &theme(), 60, strings());
        assert!(rendered.lines[0].width() < 60);
    }

    #[test]
    fn a_stamped_body_is_wrapped_to_leave_the_clock_its_columns() {
        let mut stamped = item("user", TimelineRowKind::UserMessage, "a question", false);
        stamped.timestamp_ms = Some(1_759_237_920_000);
        let unstamped = item("user", TimelineRowKind::UserMessage, "a question", false);
        let columns = clock_columns(&stamped, strings());
        assert!(columns > 0);
        assert_eq!(
            body_width_for(&stamped, 60, strings()) + columns,
            body_width_for(&unstamped, 60, strings())
        );
    }

    fn item(id: &str, kind: TimelineRowKind, body: &str, streaming: bool) -> Block {
        Block {
            id: id.to_string(),
            kind,
            title: match kind {
                TimelineRowKind::Reasoning => "Thinking".to_string(),
                TimelineRowKind::ToolCall => "execute".to_string(),
                _ => id.to_string(),
            },
            body: body.to_string(),
            turn_id: Some("turn-1".into()),
            sequence: 1,
            timestamp_ms: None,
            expanded: false,
            reasoning_window: false,
            collapsible: true,
            streaming,
            failed: false,
            pending_permission: false,
            file_path: None,
            runtime_attribution: None,
            conclusion: false,
            group: GroupRole::Solo,
            fold: None,
            activity: None,
            details: Vec::new(),
            group_summary: None,
        }
    }

    #[test]
    fn work_rows_keep_their_shape_when_the_turn_finishes() {
        // A tool row is one row whether or not it is still running: its payload
        // is a wall of JSON, and the fold is what keeps it out of the timeline.
        let mut row = item(
            "work",
            TimelineRowKind::ToolCall,
            r#"{"command":"cargo test","description":"Run tests"}"#,
            true,
        );
        let running = render_block(&row, &theme(), 60, strings());
        row.streaming = false;
        let completed = render_block(&row, &theme(), 60, strings());
        assert_eq!(running.plain, completed.plain);
        assert_eq!(running.height, 2);
        assert!(!running.text().contains("description"));
        row.expanded = true;
        let expanded = render_block(&row, &theme(), 60, strings());
        assert!(expanded.text().contains("description"));
    }

    #[test]
    fn a_thought_opens_while_it_runs_and_folds_when_it_lands() {
        let mut row = item(
            "thought",
            TimelineRowKind::Reasoning,
            "first thought\nsecond thought\nlast thought",
            true,
        );
        row.expanded = true;
        row.reasoning_window = true;
        let running = render_block(&row, &theme(), 60, strings());
        // Running: a window on the thought, under a header that says so.
        assert!(
            running.plain[0].contains("Thinking…"),
            "{:?}",
            running.plain
        );
        assert!(
            running.text().contains("last thought"),
            "{}",
            running.text()
        );
        assert_eq!(running.lines[0].spans[0].content.as_ref(), "┃");
        // Landed: one row, and the whole thought behind the fold.
        row.streaming = false;
        row.expanded = false;
        row.reasoning_window = false;
        let completed = render_block(&row, &theme(), 60, strings());
        assert_eq!(completed.height, 2);
        assert_eq!(
            completed.plain,
            vec!["▸ Thinking".to_string(), String::new()]
        );
        assert!(!completed.text().contains("first thought"));
        row.expanded = true;
        let expanded = render_block(&row, &theme(), 60, strings());
        assert!(expanded.text().contains("first thought"));
    }

    #[test]
    fn a_row_that_is_still_arriving_is_measured_as_it_is_drawn() {
        // The estimate only has to be close, but it must agree about *shape*:
        // scrolling a session of streaming rows would jump as they came into
        // view if the two disagreed.
        let mut transcript = Transcript::new();
        transcript.configure(60, &theme());
        transcript.set_blocks(vec![
            item("t1", TimelineRowKind::ToolCall, "output\nmore\nmore", true),
            // Last, so it is the thought the window belongs to.
            item(
                "r1",
                TimelineRowKind::Reasoning,
                "one\ntwo\nthree\nfour",
                true,
            ),
        ]);
        let count = transcript.blocks().len();
        let measured = (0..count)
            .map(|index| {
                transcript.measure(index, &theme(), strings());
                transcript.heights[index] as usize
            })
            .collect::<Vec<_>>();
        let estimated = (0..count)
            .map(|index| transcript.estimate_height(index))
            .collect::<Vec<_>>();
        // A tool row's shape is one row, so there the estimate is exact.
        assert_eq!(measured[0], estimated[0], "estimate and render disagree");
        // A thought's window reflows its body through markdown, which no count
        // of source lines can predict. The estimate is an upper bound instead:
        // an unmeasured window that drew taller than it was estimated would sit
        // over the rows the reader is following.
        assert!(
            estimated[1] >= measured[1],
            "the estimate understates the live window: {estimated:?} vs {measured:?}"
        );
        assert!(
            estimated[1] <= 1 + STREAMING_WINDOW_LINES + chrome::GAP,
            "the estimate exceeds the window's own bound: {estimated:?}"
        );
    }

    #[test]
    fn only_the_thought_at_the_end_of_the_session_is_live() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        // The runtime never closes a reasoning stream, so both rows still say
        // they are running; only the one the Agent is on now may open a window.
        transcript.set_blocks(vec![
            item(
                "stale",
                TimelineRowKind::Reasoning,
                "- a thought the agent has finished with",
                true,
            ),
            item("tool", TimelineRowKind::ToolCall, "ran", false),
            item(
                "live",
                TimelineRowKind::Reasoning,
                "- read the cache path\n- keep the newest rows\n- animate the rail",
                true,
            ),
        ]);
        // Nothing opens itself, so the reader opens the thought the Agent is
        // on. The older thought stays folded, and only the open one can window.
        transcript.toggle_block(2);
        let screen = transcript
            .visible_lines(ScrollState::default(), 30, &theme(), strings())
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("animate the rail"), "{screen}");
        assert!(
            !screen.contains("a thought the agent has finished with"),
            "a thought the Agent has left behind kept its window:\n{screen}"
        );
        assert_eq!(screen.matches("Thinking…").count(), 1, "{screen}");
        assert!(screen.contains("▸ Thinking"), "{screen}");
    }

    #[test]
    fn a_tool_payload_is_summarised_by_what_it_does() {
        let action = |body: &str| tool_action(body);
        assert_eq!(
            action(r#"{"command":"cargo test","timeout_ms":10}"#),
            "cargo test"
        );
        // A file operation names the file; a search names what it looks for.
        assert_eq!(action(r#"{"file_path":"/tmp/a.rs"}"#), "/tmp/a.rs");
        assert_eq!(action(r#"{"pattern":"fn main"}"#), "fn main");
        // Lists arrive as lists.
        assert_eq!(action(r#"{"command":["git","diff"]}"#), "git diff");
        // Not JSON, or nothing recognisable: the first line as it came.
        assert_eq!(action("cargo test -p vibex-tui"), "cargo test -p vibex-tui");
        assert_eq!(action(r#"{"unknown":"kept"}"#), "");
        assert_eq!(action(r#"{"command":"partial"#), "");
        assert_eq!(action("<path>/tmp/a.rs</path>"), "");
        assert_eq!(
            action("{\"command\":\"cargo test\"}\n42 tests passed"),
            "cargo test"
        );
    }

    #[test]
    fn the_agent_account_is_named_in_the_readers_language() {
        let chinese = Strings::for_locale(Locale::ZhCn);
        assert_eq!(
            runtime_attribution_line(
                chinese,
                "DeepSeek Harness · Agent account · deepseek-v4.1-flash"
            ),
            "DeepSeek Harness · Agent 账户 · deepseek-v4.1-flash"
        );
        // A transcript written before the account was renamed reads the same as
        // a new one.
        assert_eq!(
            runtime_attribution_line(
                chinese,
                "DeepSeek Harness · Default CLI account · deepseek-v4.1-flash"
            ),
            "DeepSeek Harness · Agent 账户 · deepseek-v4.1-flash"
        );
        // The hint a signed-in account carries stays behind the name.
        assert_eq!(
            runtime_attribution_line(chinese, "codex · Agent account - bai@example.com · gpt-5"),
            "codex · Agent 账户 - bai@example.com · gpt-5"
        );
        // A Profile is named by its owner, so a name that merely starts with the
        // runtime's wording is left alone.
        assert_eq!(
            runtime_attribution_line(chinese, "codex · Agent account holder · gpt-5"),
            "codex · Agent account holder · gpt-5"
        );
        assert_eq!(
            runtime_attribution_line(chinese, "codex · bai · gpt-5"),
            "codex · bai · gpt-5"
        );
    }

    #[test]
    fn attribution_is_available_in_details_without_repeating_on_collapsed_rows() {
        // Three words repeated on every row of a session is noise; the row where
        // the runtime *changes* is the one that says something.
        let attribution = Some("DeepSeek Harness · bai · deepseek-v4.1-flash".to_string());
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let mut blocks = Vec::new();
        for index in 0..3 {
            let mut row = item(
                &format!("t{index}"),
                TimelineRowKind::ToolCall,
                r#"{"command":"ls"}"#,
                false,
            );
            row.runtime_attribution = attribution.clone();
            blocks.push(row);
        }
        transcript.set_blocks(blocks);
        let lines = transcript.visible_lines(ScrollState::default(), 20, &theme(), strings());
        let named = lines
            .iter()
            .filter(|line| line.to_string().contains("DeepSeek Harness"))
            .count();
        assert_eq!(named, 0, "collapsed rows repeat runtime metadata");
        transcript.toggle_block(0);
        transcript.toggle_block(1);
        let lines = transcript.visible_lines(ScrollState::default(), 20, &theme(), strings());
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("DeepSeek Harness"))
        );

        // A switch is worth a row: the name reappears where it changes.
        let mut blocks = transcript.blocks().to_vec();
        let switched = blocks.iter().position(|block| block.id == "t2").unwrap();
        blocks[switched].runtime_attribution = Some("codex · gpt-5".to_string());
        transcript.set_blocks(blocks);
        let switched = transcript.index_of_block("t2").unwrap();
        transcript.toggle_block(switched);
        let lines = transcript.visible_lines(ScrollState::default(), 20, &theme(), strings());
        let text = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(
            text.iter().any(|line| line.contains("codex")),
            "the new runtime is not named: {text:?}"
        );
    }

    /// A turn with one work row folded away and the answer that owns the
    /// control, as the projection would hand it over.
    fn folded_turn(expanded: bool) -> Vec<Block> {
        let mut work = item("t1", TimelineRowKind::ToolCall, "the tool's output", false);
        work.fold = Some(TurnFold {
            turn_id: "turn-1".into(),
            label: "12s".into(),
            expanded,
            is_conclusion: false,
            is_head: true,
            agent_identity: None,
        });
        let mut answer = item("t2", TimelineRowKind::AgentMessage, "the answer", false);
        // The projection never makes an Agent message collapsible: an answer
        // *is* its text. The fixture has to agree, or `e` on the answer would
        // take the body branch the production block leaves to the divider.
        answer.collapsible = false;
        answer.fold = Some(TurnFold {
            turn_id: "turn-1".into(),
            label: "12s".into(),
            expanded,
            is_conclusion: true,
            is_head: false,
            agent_identity: None,
        });
        vec![work, answer]
    }

    #[test]
    fn a_folded_turn_draws_only_its_answer_and_divider() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(folded_turn(false));
        let lines = transcript.visible_lines(ScrollState::default(), 20, &theme(), strings());
        let text = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(
            !text.iter().any(|line| line.contains("the tool's output")),
            "the folded work is still drawn: {text:?}"
        );
        assert!(
            text.iter().any(|line| line.contains("the answer")),
            "the answer goes with the fold: {text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line.contains("Worked for 12s") && line.contains('▸')),
            "a shut fold is a title with a closed mark: {text:?}"
        );
    }

    /// A run of work is opened by its group head, and the head carries the
    /// turn's fold so a shut section cannot leave the run on screen. The row
    /// the head was minted from is one of the run's members, and a member that
    /// kept `is_head` drew the turn's title a second time under the head that
    /// already draws it.
    #[test]
    fn a_grouped_run_draws_its_turns_title_once() {
        let mut blocks = Vec::new();
        for index in 0..3 {
            let mut work = item(
                &format!("t{index}"),
                TimelineRowKind::ToolCall,
                "ran",
                false,
            );
            work.fold = Some(TurnFold {
                turn_id: "turn-1".into(),
                label: "12s".into(),
                expanded: true,
                is_conclusion: false,
                is_head: index == 0,
                agent_identity: None,
            });
            blocks.push(work);
        }
        let mut answer = item("t9", TimelineRowKind::AgentMessage, "the answer", false);
        answer.collapsible = false;
        answer.fold = Some(TurnFold {
            turn_id: "turn-1".into(),
            label: "12s".into(),
            expanded: true,
            is_conclusion: true,
            is_head: false,
            agent_identity: None,
        });
        blocks.push(answer);

        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(blocks);
        assert!(
            transcript.blocks()[0].group.is_head(),
            "the three rows are one group"
        );
        // The group opens onto its members, which is when the run's own rows
        // are on screen at all.
        assert!(transcript.toggle_block(0));
        transcript.advance_disclosures(Instant::now() + DISCLOSURE_DURATION);
        let lines = transcript.visible_lines(ScrollState::default(), 20, &theme(), strings());
        let text = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        // The section's title carries the open disclosure; the rule that closes
        // it carries the fold mark, so only the first is counted here.
        let titles = text
            .iter()
            .filter(|line| line.contains('▾') && line.contains("Worked for 12s"))
            .count();
        assert_eq!(
            titles, 1,
            "the turn's title is drawn more than once: {text:?}"
        );
    }

    #[test]
    fn opening_a_fold_draws_the_work_between_a_title_and_a_rule() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(folded_turn(true));
        let lines = transcript.visible_lines(ScrollState::default(), 20, &theme(), strings());
        let text = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert!(
            text.iter().any(|line| line.contains("the tool's output")),
            "the open fold hides the work it stands for: {text:?}"
        );
        // Open, the section is bracketed: the title above the work, with the
        // mark that puts it away pointing down into it, and the rule below,
        // pointing back up at what it closes.
        let title = text
            .iter()
            .position(|line| line.contains("Worked for 12s"))
            .expect("the title is drawn");
        assert!(text[title].contains('▾'), "{:?}", text[title]);
        let work = text
            .iter()
            .position(|line| line.contains("the tool's output"))
            .expect("the work is drawn");
        assert!(title < work, "the title is below the work: {text:?}");
        let rule = text
            .iter()
            .position(|line| line.contains('▴'))
            .expect("the rule is drawn");
        assert!(rule > work, "the rule is above the work: {text:?}");
        assert!(
            text[rule].contains("─ Worked for 12s ─"),
            "the rule does not carry the duration between its rules: {:?}",
            text[rule]
        );
        // Both ends are the control, so a press on either one folds the turn.
        let work_index = transcript
            .blocks()
            .iter()
            .position(|block| block.fold.as_ref().is_some_and(|fold| fold.is_head))
            .expect("the work row opens the section");
        assert_eq!(
            transcript.fold_control_lines(work_index),
            vec![title],
            "the title does not fold the turn"
        );
        let answer = transcript
            .blocks()
            .iter()
            .position(|block| block.fold.as_ref().is_some_and(TurnFold::is_control))
            .expect("the answer owns the control");
        assert_eq!(
            transcript.fold_control_lines(answer),
            vec![rule],
            "the rule does not fold the turn"
        );
    }

    #[test]
    fn a_fold_row_is_separated_from_what_follows_it() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(folded_turn(false));
        let lines = transcript.visible_lines(ScrollState::default(), 20, &theme(), strings());
        let text = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        let title = text
            .iter()
            .position(|line| line.contains("Worked for 12s"))
            .expect("the title is drawn");
        let answer = text
            .iter()
            .position(|line| line.contains("the answer"))
            .expect("the answer is drawn");
        assert_eq!(
            answer,
            title + 2,
            "the answer is drawn against the fold row: {text:?}"
        );
        assert!(
            text[title + 1].trim().is_empty(),
            "the row between the title and the answer is not blank: {:?}",
            text[title + 1]
        );
    }

    #[test]
    fn a_selected_fold_lights_its_own_row_and_not_the_answer() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(folded_turn(false));
        let answer = transcript
            .blocks()
            .iter()
            .position(|block| block.fold.as_ref().is_some_and(TurnFold::is_control))
            .expect("the answer owns the control");
        let scroll = ScrollState {
            selected: Some(answer),
            ..Default::default()
        };
        let lines = transcript.visible_lines(scroll, 20, &theme(), strings());
        let highlighted = lines
            .iter()
            .position(|line| line.style.bg == Some(theme().roles.surface_highlight))
            .expect("the current block is lifted");
        assert!(
            lines[highlighted].to_string().contains("Worked for 12s"),
            "the cursor left the fold row: {:?}",
            lines[highlighted].to_string()
        );
        assert_eq!(
            lines[highlighted].spans[0].content.as_ref(),
            "▌",
            "the fold row does not carry the pointer"
        );
        assert!(
            lines[highlighted + 1..]
                .iter()
                .all(|line| line.style.bg != Some(theme().roles.surface_highlight)),
            "the answer's first row kept the cursor"
        );
    }

    #[test]
    fn a_fold_title_reserves_a_slot_for_the_agents_mark() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        let mut blocks = folded_turn(false);
        for block in &mut blocks {
            if let Some(fold) = block.fold.as_mut() {
                fold.agent_identity = Some("codex".into());
            }
        }
        // A terminal that cannot hold a picture keeps the words alone: the slot
        // would be air, and the title would indent for nothing.
        transcript.set_blocks(blocks.clone());
        let plain = transcript.visible_lines(ScrollState::default(), 20, &theme(), strings());
        let title = plain
            .iter()
            .find(|line| line.to_string().contains("Worked for 12s"))
            .expect("the title is drawn")
            .to_string();
        assert!(transcript.window_marks().is_empty());
        // The margin and the pad come first on every transcript row, so the
        // title's own row starts two cells in.
        assert!(title.trim_start().starts_with("▸  Worked for"), "{title:?}");

        transcript.set_pictures(true);
        let lines = transcript.visible_lines(ScrollState::default(), 20, &theme(), strings());
        let title = lines
            .iter()
            .find(|line| line.to_string().contains("Worked for 12s"))
            .expect("the title is drawn")
            .to_string();
        let mark = transcript
            .window_marks()
            .first()
            .expect("the title asks for the Agent's mark");
        assert_eq!(mark.identity, "codex");
        assert_eq!(mark.column, 4, "the mark does not sit after the disclosure");
        // The label keeps its column, so the words after the picture line up
        // whether or not the terminal drew it.
        assert!(
            title.trim_start().starts_with("▸    Worked for"),
            "{title:?}"
        );
    }

    #[test]
    fn a_fold_toggle_keeps_the_heights_the_turn_measured() {
        // Opening or shutting a turn's process section changes which rows are
        // drawn, not how tall its answer is. Re-measuring the turn would put a
        // long answer back on its estimate — which is nowhere near the rows
        // markdown actually draws — and a reader parked in the section would be
        // dropped by the distance between the two.
        let long = (1..=12)
            .map(|line| format!("paragraph {line}\n"))
            .collect::<String>();
        let mut work = item("t1", TimelineRowKind::ToolCall, "ran", false);
        work.fold = Some(TurnFold {
            turn_id: "turn-1".into(),
            label: "12s".into(),
            expanded: false,
            is_conclusion: false,
            is_head: true,
            agent_identity: None,
        });
        let mut answer = item("t2", TimelineRowKind::AgentMessage, &long, false);
        answer.collapsible = false;
        answer.fold = Some(TurnFold {
            turn_id: "turn-1".into(),
            label: "12s".into(),
            expanded: false,
            is_conclusion: true,
            is_head: false,
            agent_identity: None,
        });
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(vec![work, answer]);
        let control = transcript
            .blocks()
            .iter()
            .position(|block| block.fold.as_ref().is_some_and(TurnFold::is_control))
            .expect("the answer owns the control");
        // A frame measures the shut layout; the toggles below run without one,
        // so what they read is what the fold left behind.
        let _ = transcript.visible_lines(ScrollState::default(), 40, &theme(), strings());
        let shut = transcript.total_height();
        assert!(transcript.toggle_block(control));
        let open = transcript.total_height();
        assert!(transcript.toggle_block(control));
        assert!(
            open > shut,
            "the open section does not draw the work it stands for"
        );
        assert_eq!(
            transcript.total_height(),
            shut,
            "the fold toggle put the turn back on its estimates"
        );
    }

    #[test]
    fn a_fold_choice_survives_a_rebuild() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(folded_turn(false));
        let control = transcript
            .blocks()
            .iter()
            .position(|block| block.fold.as_ref().is_some_and(TurnFold::is_control))
            .expect("the answer owns the control");
        assert!(transcript.toggle_block(control));
        assert!(
            transcript.blocks()[0]
                .fold
                .as_ref()
                .is_some_and(|f| f.expanded)
        );

        // The projection hands the fold back shut on the next delta; the
        // reader's choice is re-applied rather than discarded.
        transcript.set_blocks(folded_turn(false));
        assert!(
            transcript.blocks()[0]
                .fold
                .as_ref()
                .is_some_and(|f| f.expanded),
            "an explicit expansion was undone by a rebuild"
        );

        // A fold nothing refers to any more takes its choice with it.
        transcript.set_blocks(vec![item(
            "solo",
            TimelineRowKind::AgentMessage,
            "hi",
            false,
        )]);
        assert!(transcript.turn_expansion.is_empty());
    }

    #[test]
    fn a_folded_block_is_skipped_by_the_cursor_and_search() {
        let mut transcript = Transcript::new();
        transcript.configure(80, &theme());
        transcript.set_blocks(folded_turn(false));
        // The hidden work resolves to the control that stands for it, so the
        // cursor never lands on a row that is not drawn.
        assert_eq!(transcript.visible_block(0), Some(1));
        // And a search hit inside the fold opens it before revealing itself.
        transcript.reveal_block(0);
        assert!(
            transcript.blocks()[0]
                .fold
                .as_ref()
                .is_some_and(|f| f.expanded),
            "revealing a hidden row left the fold shut"
        );
    }
}

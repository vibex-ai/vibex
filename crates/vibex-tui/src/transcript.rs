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

use std::collections::HashMap;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use vibex_desktop_model::TimelineRowKind;

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
/// Upper bound on the blocks a single transcript keeps, mirroring the shared
/// controller's own timeline budget.
pub const MAX_BLOCKS: usize = 20_000;

const UNMEASURED: u32 = u32::MAX;

/// The shortest run of collapsed work items worth folding.
pub const MIN_GROUP_RUN: usize = 3;

/// The most body rows a live thought shows before it folds.
///
/// A thought arrives a token at a time, and a block that grew with every token
/// would push the rest of the session off the screen while the reader is
/// reading it. The window is the compromise: a fixed number of rows at the
/// tail, so the newest lines push the oldest off the top instead of adding
/// height. One of the rows is the fold marker once the thought outgrows the
/// window, so the block is never taller than this plus its header.
pub const STREAMING_WINDOW_LINES: usize = 6;

/// The mark that says older rows of a live thought are above the window.
///
/// A character rather than a chevron: the window's oldest row is *content*, and
/// a glyph that pointed at a fold would read as a control the reader can press.
const FOLD_MARKER: &str = "…";

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
    block.kind == TimelineRowKind::Reasoning && block.streaming && !block.expanded
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
/// the animation, the fold marker, the bound on its height — is gated on this
/// one predicate.
pub fn is_live_thinking_window(block: &Block, trailing: bool) -> bool {
    trailing && is_running_thought(block)
}

/// Whether a block can be folded into a dense run.
///
/// Only collapsed work items qualify: an expanded block is one the reader asked
/// to see, and a message is never chrome. A live thought does not qualify
/// either: folding it into a run would hide the window the reader is watching.
fn eligible_for_group(block: &Block, trailing: bool) -> bool {
    !is_live_thinking_window(block, trailing)
        && is_work_item(block.kind)
        && block.collapsible
        && !block.expanded
        && !block.failed
}

/// Whether `incoming` starts with older blocks than the transcript holds.
///
/// True when the transcript's current head survives somewhere after index 0,
/// which is what a prepend looks like after the diff. An empty transcript is
/// not a prepend, and neither is a reload whose head moved to the front.
fn prepends_existing_blocks(existing: &[Block], incoming: &[Block]) -> bool {
    let Some(head) = existing.first() else {
        return false;
    };
    incoming
        .iter()
        .position(|block| block.id == head.id)
        .is_some_and(|index| index > 0)
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
    pub expanded: bool,
    pub collapsible: bool,
    pub streaming: bool,
    pub failed: bool,
    pub pending_permission: bool,
    pub file_path: Option<String>,
    pub runtime_attribution: Option<String>,
    pub conclusion: bool,
    /// How this block participates in a dense run of work items.
    pub group: GroupRole,
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
        self.streaming.hash(&mut hasher);
        self.failed.hash(&mut hasher);
        self.pending_permission.hash(&mut hasher);
        self.collapsible.hash(&mut hasher);
        self.file_path.hash(&mut hasher);
        self.group.hash(&mut hasher);
        self.runtime_attribution.hash(&mut hasher);
        hasher.finish()
    }

    /// Whether the block shows its whole body.
    pub fn is_open(&self) -> bool {
        self.expanded || !self.collapsible
    }
}

/// A rendered block: its display lines and the plain text of each.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderedBlock {
    pub lines: Vec<Line<'static>>,
    pub plain: Vec<String>,
    pub height: usize,
}

impl RenderedBlock {
    pub fn text(&self) -> String {
        self.plain.join("\n")
    }
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
    /// Counters that make the cache behaviour observable in tests and in the
    /// benchmark harness.
    pub stats: TranscriptStats,
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
            stats: TranscriptStats::default(),
        }
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
        // Trim when the authority's budget is exceeded. A prepend is the one
        // case where the incoming list starts with history the reader just
        // asked for, so dropping from the front would delete exactly what the
        // fetch was for; an append or a wholesale reload keeps the newest
        // blocks instead, which is the behaviour that predates the cursor.
        let blocks = if blocks.len() > MAX_BLOCKS {
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

        let mut next_blocks = Vec::with_capacity(blocks.len());
        let mut next_heights = Vec::with_capacity(blocks.len());
        let mut next_keys = Vec::with_capacity(blocks.len());
        let mut reused_rendered: HashMap<usize, RenderedBlock> = HashMap::new();
        let mut reused_recency = Vec::new();

        for (new_index, mut block) in blocks.into_iter().enumerate() {
            if let Some(&old_index) = existing.get(block.id.as_str()) {
                block.expanded = self.blocks[old_index].expanded;
                block.group = self.blocks[old_index].group;
            }
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
        self.heights = next_heights;
        self.keys = next_keys;
        self.rendered = reused_rendered;
        self.recency = reused_recency;
        self.apply_grouping();
        self.layout_valid = false;
        change
    }

    /// Fold long runs of collapsed work items into their first member.
    ///
    /// A session produces work items in bursts — ten file reads, six greps —
    /// and showing all of them at full height buries the sentences they are
    /// evidence for. A run of three or more collapsed items keeps its first
    /// member and reports the rest as a count, which is the density the reader
    /// wants by default and one keypress away from the detail.
    fn apply_grouping(&mut self) {
        let previous = self
            .blocks
            .iter()
            .map(|block| block.group)
            .collect::<Vec<_>>();
        for block in &mut self.blocks {
            block.group = GroupRole::Solo;
        }
        let mut index = 0usize;
        while index < self.blocks.len() {
            // Only the final row can be a live window: nothing follows it, so
            // it is the thought the Agent is on rather than one it has left.
            let trailing = index + 1 == self.blocks.len();
            if !eligible_for_group(&self.blocks[index], trailing) {
                index += 1;
                continue;
            }
            let start = index;
            let kind = self.blocks[start].kind;
            let title = self.blocks[start].title.clone();
            let turn = self.blocks[start].turn_id.clone();
            // A run is one kind of work *by one runtime*: folding a row that
            // came from somewhere else into this run would hide the only thing
            // the attribution is there to say.
            let attribution = self.blocks[start].runtime_attribution.clone();
            while index < self.blocks.len()
                && eligible_for_group(&self.blocks[index], index + 1 == self.blocks.len())
                && self.blocks[index].kind == kind
                && self.blocks[index].title == title
                && self.blocks[index].turn_id == turn
                && self.blocks[index].runtime_attribution == attribution
            {
                index += 1;
            }
            let run = index - start;
            // Two in a row still read as a pair; three is where a run starts to
            // cost more rows than it earns.
            if run < MIN_GROUP_RUN {
                continue;
            }
            let hidden = run - 1;
            self.blocks[start].group = GroupRole::Head { hidden };
            for member in &mut self.blocks[start + 1..index] {
                member.group = GroupRole::Member;
            }
        }
        for (index, old) in previous.into_iter().enumerate() {
            if old != self.blocks[index].group {
                self.keys[index] = self.blocks[index].content_key();
                self.invalidate(index);
                if index > 0 {
                    self.invalidate(index - 1);
                }
            }
        }
    }

    /// How many turns the transcript contains.
    ///
    /// A turn is a run of blocks sharing a `turn_id`; blocks without one are
    /// grouped into a single implicit turn so the rail always has an answer.
    pub fn turn_count(&self) -> usize {
        let mut seen: Vec<&str> = Vec::new();
        for block in &self.blocks {
            let key = block.turn_id.as_deref().unwrap_or("");
            if !seen.contains(&key) {
                seen.push(key);
            }
        }
        seen.len().max(usize::from(!self.blocks.is_empty()))
    }

    /// The turn the viewport top is on, for the rail's current-tick marker.
    pub fn active_turn(&mut self) -> Option<usize> {
        self.ensure_layout();
        let offset = self.scroll_offset;
        let index = self.block_at_line(offset)?;
        let key = self.blocks.get(index)?.turn_id.clone();
        let mut turn = 0usize;
        let mut seen: Vec<Option<String>> = Vec::new();
        for block in &self.blocks {
            if !seen.contains(&block.turn_id) {
                if block.turn_id == key {
                    return Some(turn);
                }
                seen.push(block.turn_id.clone());
                turn += 1;
            }
        }
        Some(0)
    }

    /// The first block of the `turn`-th turn, for a click on the rail.
    pub fn block_of_turn(&self, turn: usize) -> Option<usize> {
        let mut seen: Vec<Option<&str>> = Vec::new();
        for (index, block) in self.blocks.iter().enumerate() {
            let key = block.turn_id.as_deref();
            if !seen.contains(&key) {
                if seen.len() == turn {
                    return Some(index);
                }
                seen.push(key);
            }
        }
        None
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
        self.blocks.iter().any(|block| block.streaming)
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
        self.blocks
            .iter()
            .rposition(|block| block.group != GroupRole::Member)
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
    pub fn toggle_block(&mut self, index: usize) -> bool {
        let Some(block) = self.blocks.get_mut(index) else {
            return false;
        };
        if !block.collapsible {
            return false;
        }
        let expanded = !block.expanded;
        let count = match block.group {
            GroupRole::Head { hidden } => hidden + 1,
            _ => 1,
        };
        for member in index..(index + count).min(self.blocks.len()) {
            self.blocks[member].expanded = expanded;
            self.keys[member] = self.blocks[member].content_key();
            self.invalidate(member);
        }
        self.apply_grouping();
        if index > 0 {
            self.invalidate(index - 1);
        }
        true
    }

    /// Expand or collapse every collapsible block at once.
    pub fn toggle_all(&mut self, expanded: bool) {
        for index in 0..self.blocks.len() {
            if !self.blocks[index].collapsible {
                continue;
            }
            if self.blocks[index].expanded != expanded {
                self.blocks[index].expanded = expanded;
                self.keys[index] = self.blocks[index].content_key();
                self.invalidate(index);
                if index > 0 {
                    self.invalidate(index - 1);
                }
            }
        }
        self.apply_grouping();
    }

    /// Whether every collapsible block is currently open.
    pub fn all_expanded(&self) -> bool {
        self.blocks
            .iter()
            .filter(|block| block.collapsible)
            .all(|block| block.expanded)
    }

    fn invalidate(&mut self, index: usize) {
        if let Some(height) = self.heights.get_mut(index) {
            *height = UNMEASURED;
        }
        self.rendered.remove(&index);
        self.recency.retain(|value| *value != index);
        self.layout_valid = false;
    }

    fn next_visible_block(&self, index: usize) -> Option<&Block> {
        self.blocks
            .iter()
            .skip(index + 1)
            .find(|block| block.group != GroupRole::Member)
    }

    fn ensure_layout(&mut self) {
        if self.layout_valid {
            return;
        }
        self.offsets.clear();
        self.offsets.reserve(self.blocks.len() + 1);
        let mut total = 0usize;
        self.offsets.push(0);
        for (index, height) in self.heights.iter().enumerate() {
            let height = match *height {
                UNMEASURED => {
                    // A cheap estimate keeps scroll maths stable without
                    // wrapping text the user will never see.
                    self.estimate_height(index)
                }
                measured => measured as usize,
            };
            total += height;
            self.offsets.push(total);
        }
        self.layout_valid = true;
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
        if matches!(block.group, GroupRole::Member) {
            return 0;
        }
        let next = self.next_visible_block(index);
        let gap = gap_after(block, next);
        let available = self.width.max(8);
        let dense = is_dense_row(block.kind);
        let open = if dense {
            block.expanded
        } else {
            block.is_open()
        };
        // The header is truncated to one row, and a message has none at all —
        // its first line *is* the message.
        let headerless = matches!(
            block.kind,
            TimelineRowKind::UserMessage | TimelineRowKind::AgentMessage
        ) && !block.body.is_empty();
        let header = usize::from(!headerless);
        let body_lines = if block.body.is_empty() {
            0
        } else if is_live_thinking_window(block, next.is_none()) {
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
        (header + body_lines + status + gap).max(1)
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
        let Some(block) = self.blocks.get(index).cloned() else {
            return RenderedBlock::default();
        };
        // The successor decides the separator, so a run of tool calls renders
        // as a list rather than as a stack of sections.
        let next = self.next_visible_block(index).cloned();
        let prose = if matches!(block.kind, TimelineRowKind::UserMessage) {
            theme.base()
        } else {
            theme.prose()
        };
        let body_width = chrome::content_width(self.width.max(8)).max(8);
        // Only a body that will be drawn is worth parsing as it arrives: a
        // dense row shows one live line, and re-rendering the whole thought to
        // keep a renderer it never reads is work the session does not need.
        // A live window always draws, so its renderer is always advanced —
        // which is what keeps a running thought's cost proportional to what is
        // on screen rather than to what has been thought.
        let body_shown = !block.body.is_empty()
            && (!is_dense_row(block.kind)
                || block.expanded
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
        render_block_with_attribution(
            &block,
            next.as_ref(),
            streamed,
            theme,
            self.width,
            strings,
            self.last_selected == Some(index),
            show_attribution,
            self.phase,
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
                for line in rendered.lines.iter().skip(skip).take(take) {
                    lines.push(line.clone());
                }
            } else {
                // Not resident: rebuild it for this frame.
                let rendered = self.render_block(index, theme, strings);
                for line in rendered.lines.iter().skip(skip).take(take) {
                    lines.push(line.clone());
                }
                self.store_rendered(index, rendered);
            }
            index += 1;
        }
        if lines.is_empty() && !self.blocks.is_empty() && offset < total {
            self.stats.idle_frames += 1;
        }
        lines
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
    /// truncated to its pinned height, and it is pushed off by the next prompt
    /// rather than overlapped, so the transcript below is never hidden.
    ///
    /// `viewport` is the height the transcript *would* have without a header;
    /// the caller passes a conservatively small value so the decision cannot
    /// oscillate between two frames.
    pub fn sticky_header(
        &mut self,
        scroll: ScrollState,
        viewport: usize,
        theme: &TuiTheme,
        strings: Strings,
    ) -> Option<StickyHeader> {
        if viewport < 3 || self.blocks.is_empty() {
            return None;
        }
        if scroll.follow {
            // One screen plus a pinned header's worth: the prompt that is about
            // to be pinned sits just above the viewport, and a header can only
            // be clipped from a block that has been measured.
            self.measure_tail(viewport + MAX_STICKY_ROWS + STICKY_GAP_ROWS, theme, strings);
        }
        self.ensure_layout();
        let total = self.offsets.last().copied().unwrap_or(0);
        let offset = if scroll.follow {
            total.saturating_sub(viewport)
        } else {
            scroll.offset.min(total.saturating_sub(1))
        };
        if offset == 0 {
            return None;
        }
        // The last prompt the reader has scrolled past.
        let mut candidate = None;
        for index in 0..self.blocks.len() {
            let start = self.offsets.get(index).copied().unwrap_or(0);
            if start >= offset {
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
        // readable however far the reader has scrolled.
        let scroll_past = offset.saturating_sub(start);
        let floor = full_height.clamp(1, MAX_STICKY_ROWS);
        let mut height = full_height
            .saturating_sub(scroll_past)
            .max(floor)
            .min(viewport.saturating_sub(1));
        if height == 0 {
            return None;
        }
        let mut clip_top = 0usize;
        // The next prompt pushes this one off, from the bottom up.
        for next in index + 1..self.blocks.len() {
            let next_start = self.offsets.get(next).copied().unwrap_or(0);
            if next_start <= offset {
                continue;
            }
            if self.blocks[next].kind != TimelineRowKind::UserMessage || !self.is_sticky(next) {
                continue;
            }
            let naive = next_start - offset;
            if naive <= height + STICKY_GAP_ROWS {
                let visible = naive.saturating_sub(1);
                if visible == 0 {
                    return None;
                }
                // The pushed header reveals its bottom rows, clipped from the
                // truncated header rather than from the whole prompt.
                let pushed_height = full_height.min(height);
                clip_top = pushed_height.saturating_sub(visible);
                height = visible;
            }
            break;
        }
        let rendered = self.rendered.get(&index)?.clone();
        // The block's own trailing blank rows are the separator before the next
        // block, not content: a header clipped down to them would reserve a row
        // to show nothing, so the clip stops at the last row with text on it.
        let content_rows = rendered
            .plain
            .iter()
            .rposition(|line| !line.trim().is_empty())
            .map_or(0, |last| last + 1);
        if content_rows == 0 {
            return None;
        }
        let clip_top = clip_top.min(content_rows - 1);
        let end = (clip_top + height).min(content_rows);
        let lines = rendered
            .lines
            .get(clip_top.min(end)..end)
            .unwrap_or_default()
            .to_vec();
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
        let mut covered = 0usize;
        let mut index = self.blocks.len();
        while index > 0 && covered < height {
            index -= 1;
            if self.heights.get(index).copied() == Some(UNMEASURED) {
                covered += self.measure(index, theme, strings) as usize;
            } else {
                covered += self.heights[index] as usize;
            }
        }
    }

    fn block_range(&mut self, index: usize) -> (usize, usize) {
        self.ensure_layout();
        let start = self.offsets.get(index).copied().unwrap_or(0);
        let height = self
            .heights
            .get(index)
            .map(|value| match *value {
                UNMEASURED => self.estimate_height(index),
                measured => measured as usize,
            })
            .unwrap_or(0);
        (start, start + height)
    }

    /// Plain text of the whole transcript, used by copy and search.
    pub fn plain_text(&mut self) -> String {
        let mut output = String::new();
        for index in 0..self.blocks.len() {
            if index > 0 {
                output.push('\n');
            }
            output.push_str(&block_plain_text(&self.blocks[index]));
        }
        output
    }

    /// Plain text of one block.
    pub fn block_text(&self, index: usize) -> Option<String> {
        self.blocks.get(index).map(block_plain_text)
    }

    /// The plain text of a range of display lines.
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
    ) -> Vec<String> {
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
                    lines.push(line.clone());
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
                block.title.to_lowercase().contains(&query)
                    || block.body.to_lowercase().contains(&query)
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
            let hits = pattern.count(&block.title) + pattern.count(&block.body);
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
fn block_plain_text(block: &Block) -> String {
    let mut output = String::new();
    if !block.title.is_empty() {
        output.push_str(&block.title);
    }
    if !block.body.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&block.body);
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
    /// Heads a collapsed run. `hidden` members follow it invisibly.
    Head { hidden: usize },
    /// Collapsed into the run above; contributes no rows.
    Member,
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
    let dense_run = is_dense_row(block.kind)
        && !block.expanded
        && next.is_some_and(|next| is_dense_row(next.kind) && !next.expanded);
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
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut plain: Vec<String> = Vec::new();

    if matches!(block.group, GroupRole::Member) {
        // Folded into the run above; contributes nothing but is still counted
        // by the head's summary.
        return RenderedBlock::default();
    }

    let content_width = chrome::content_width(width).max(8);
    let label = kind_label(block.kind, strings);
    let dense = is_dense_row(block.kind);
    // A running thought the reader has not opened, at the end of the session:
    // the one block whose body is drawn without an explicit expansion, inside a
    // window on its tail.
    let live = is_live_thinking_window(block, next.is_none());
    // `Block::is_open` means "not collapsible" as well as "the reader opened
    // it"; a dense row is showing one line because that is its shape, so only
    // an explicit expansion reveals its body.
    let open = if dense {
        block.expanded
    } else {
        block.is_open()
    };
    let title_style = match block.kind {
        TimelineRowKind::UserMessage => theme.base().add_modifier(Modifier::BOLD),
        _ => theme.base(),
    };
    let mut parts: Vec<(String, Style)> = Vec::new();
    if let Some((glyph, style)) = bullet(block, live, theme) {
        parts.push((format!("{glyph} "), style));
    }
    if shows_kind_label(block.kind, &block.title, label) {
        parts.push((
            format!("{label} "),
            theme.dimmed(theme.roles.gray).add_modifier(Modifier::BOLD),
        ));
    }
    // A notice's title is its severity and its body is the sentence, so the
    // sentence is what the row shows — with the severity carried by colour
    // rather than by a word the reader has to skip.
    let (heading, title_style) = if matches!(block.kind, TimelineRowKind::SystemNotice) {
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
    if let GroupRole::Head { hidden } = block.group
        && hidden > 0
    {
        // Before the summary: a truncated row must still say how many rows it
        // stands for.
        parts.push((format!("  +{hidden}"), theme.dimmed(theme.roles.gray_dim)));
    }
    if dense
        && !open
        && !matches!(block.kind, TimelineRowKind::SystemNotice)
        && let Some(summary) = dense_summary(block)
    {
        parts.push((
            format!(
                "  {}",
                summary.split_whitespace().collect::<Vec<_>>().join(" ")
            ),
            theme.muted(),
        ));
    }
    if dense && block.failed {
        parts.push((format!("  {}", strings.failed()), theme.danger()));
    }
    if block.collapsible && !block.expanded && !dense {
        parts.push((
            format!("  ({})", strings.transcript_collapsed_hint()),
            theme.dimmed(theme.roles.gray_dim),
        ));
    }
    let body = block.body.trim_end_matches('\n');
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
    // The mark costs two columns, so the text is wrapped that much narrower.
    let body_width = content_width
        .saturating_sub(prompt_mark.map_or(0, |_| 2))
        .max(8);

    // A dense row is one row: its body is what the fold is for, and the detail
    // overlay can show all of it. Messages render every arriving line; work
    // records keep the same shape until the reader expands them — except a
    // running thought, whose whole reason to be on screen is that the reader
    // watches it arrive.
    let shows_body = !body.is_empty() && (!dense || open || live);
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
    // the top instead of the block growing without bound. The rail is then one
    // cell per row of the window, its header included, so the bar the reader
    // sees is exactly as tall as the range it stands for.
    if let Some(rendered) = rendered.as_mut()
        && live
    {
        fold_live_window(rendered, theme);
    }

    if !headerless {
        let header = truncate_parts(parts, content_width);
        let header_plain = header
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let mut header_line = row_line(row_marker(0, live, selected, phase, theme), header);
        if selected {
            header_line = header_line.style(Style::default().bg(theme.roles.surface_highlight));
        }
        lines.push(header_line);
        plain.push(header_plain);
    }

    if let Some(rendered) = rendered {
        let body_background = body_band(block, theme);
        let style = body_style(block, theme);
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
            // The pointer is the first body row's marker only where there is no
            // rail to give it up: a live window's rail runs unbroken, and the
            // header above already carries the cursor.
            let pointer = selected && index == 0 && !live;
            let mut row = row_line(row_marker(index + 1, live, pointer, phase, theme), spans)
                .style(row_style);
            if index == 0 && selected {
                row = row.style(Style::default().bg(theme.roles.surface_highlight));
            }
            lines.push(row);
            plain.push(text);
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
    let attributed =
        show_attribution && block.kind != TimelineRowKind::UserMessage && block.expanded;
    if let Some(runtime) = block.runtime_attribution.as_ref().filter(|_| attributed) {
        status.push((format!("[{runtime}]"), theme.muted()));
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
    for _ in 0..gap_after(block, next) {
        lines.push(row_line(Span::raw(" "), Vec::new()));
        plain.push(String::new());
    }

    RenderedBlock {
        height: lines.len(),
        lines,
        plain,
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
/// screen as a thought ten lines long. Once rows have left, the top row says so
/// — without it the reader would take the first visible line for the beginning
/// of the thought.
fn fold_live_window(rendered: &mut crate::markdown::RenderedMarkdown, theme: &TuiTheme) {
    let window = STREAMING_WINDOW_LINES.max(2);
    if rendered.lines.len() > window {
        let keep = window - 1;
        let cut = rendered.lines.len() - keep;
        rendered.lines.drain(..cut);
        rendered.plain.drain(..cut);
        rendered.lines.insert(
            0,
            Line::from(Span::styled(FOLD_MARKER, theme.dimmed(theme.roles.gray))),
        );
        rendered.plain.insert(0, FOLD_MARKER.to_string());
    }
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
            expanded: false,
            collapsible: true,
            streaming: false,
            failed: false,
            pending_permission: false,
            file_path: None,
            runtime_attribution: None,
            conclusion: false,
            group: GroupRole::Solo,
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
            "- read the cache path\n- check how a delta invalidates it\n- decide where the window lives\n- keep the newest rows\n- drop the oldest\n- mark the fold\n- draw the rail\n- animate the rail",
        );
        entry.streaming = true;
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
        // The newest rows are the ones shown, the oldest have left the top, and
        // the row above them says so.
        assert!(text.contains("animate the rail"), "{text}");
        assert!(text.contains(FOLD_MARKER), "{text}");
        assert!(!text.contains("read the cache path"), "{text}");
        // The window is bounded: header, window rows, gap.
        assert!(
            rendered.height <= 1 + STREAMING_WINDOW_LINES + chrome::GAP,
            "the window grew past its bound: {} rows",
            rendered.height
        );
        assert!(
            rendered.plain[1] == FOLD_MARKER,
            "the fold marker is not the first row of the window: {:?}",
            rendered.plain[1]
        );
    }

    #[test]
    fn a_thought_that_has_landed_folds_to_one_row() {
        let mut entry = running_thought();
        entry.streaming = false;
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
            GroupRole::Head { hidden: 5 }
        ));
        for entry in &transcript.blocks()[1..] {
            assert_eq!(entry.group, GroupRole::Member);
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
        assert!(transcript.toggle_block(0));
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
        assert!(rendered.plain[0].contains("# heading  **literal**"));
        assert!(rendered.plain[1].starts_with("  第二行"));
        for line in rendered.lines.iter().take(rendered.height - 1) {
            assert_eq!(line.style.bg, Some(theme().roles.surface_raised));
            assert!(line.width() <= 30);
        }
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
            expanded: false,
            collapsible: true,
            streaming,
            failed: false,
            pending_permission: false,
            file_path: None,
            runtime_attribution: None,
            conclusion: false,
            group: GroupRole::Solo,
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
        let lines = transcript.visible_lines(ScrollState::default(), 20, &theme(), strings());
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("DeepSeek Harness"))
        );

        // A switch is worth a row: the name reappears where it changes.
        let mut blocks = transcript.blocks().to_vec();
        blocks[2].runtime_attribution = Some("codex · gpt-5".to_string());
        transcript.set_blocks(blocks);
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
}

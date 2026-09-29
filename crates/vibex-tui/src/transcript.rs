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
use vibex_desktop_model::TimelineRowKind;

use crate::locale::Strings;
use crate::markdown::{render_markdown, render_plain};
use crate::text::{display_width, truncate_to_width};
use crate::theme::TuiTheme;

/// How many collapsed lines a long block shows before folding.
pub const COLLAPSED_BODY_LINES: usize = 4;
/// How many rendered blocks stay resident. Blocks outside the window keep their
/// measured height but drop their styled lines.
pub const RENDER_CACHE_BLOCKS: usize = 192;
/// Upper bound on the blocks a single transcript keeps, mirroring the shared
/// controller's own timeline budget.
pub const MAX_BLOCKS: usize = 20_000;

const UNMEASURED: u32 = u32::MAX;

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

/// Viewport scroll state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollState {
    /// Offset in display lines from the top of the transcript.
    pub offset: usize,
    /// Whether the view sticks to the bottom as new content arrives.
    pub follow: bool,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            offset: 0,
            follow: true,
        }
    }
}

/// The transcript model.
pub struct Transcript {
    blocks: Vec<Block>,
    /// Per-block measured height, or [`UNMEASURED`].
    heights: Vec<u32>,
    /// Per-block content key at the time the height was measured.
    keys: Vec<u64>,
    /// Prefix sums over `heights`, using the estimate for unmeasured slots.
    offsets: Vec<usize>,
    layout_valid: bool,
    rendered: HashMap<usize, RenderedBlock>,
    /// Recency order for the render cache, oldest first.
    recency: Vec<usize>,
    width: usize,
    theme_id: String,
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
            offsets: Vec::new(),
            layout_valid: false,
            rendered: HashMap::new(),
            recency: Vec::new(),
            width: 0,
            theme_id: String::new(),
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

    pub fn block(&self, index: usize) -> Option<&Block> {
        self.blocks.get(index)
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
        // Trim from the front when the authority's budget is exceeded.
        let blocks = if blocks.len() > MAX_BLOCKS {
            change.removed += blocks.len() - MAX_BLOCKS;
            blocks[blocks.len() - MAX_BLOCKS..].to_vec()
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

        self.blocks = next_blocks;
        self.heights = next_heights;
        self.keys = next_keys;
        self.rendered = reused_rendered;
        self.recency = reused_recency;
        self.layout_valid = false;
        change
    }

    /// Tell the transcript which width and theme it will render at.
    ///
    /// A change invalidates every measurement, because wrapping is width
    /// dependent. Callers normally pass the same values every frame, in which
    /// case this is free.
    pub fn configure(&mut self, width: usize, theme: &TuiTheme) {
        let width = width.max(8);
        let theme_id = theme.id.to_string();
        if self.width == width && self.theme_id == theme_id {
            return;
        }
        self.width = width;
        self.theme_id = theme_id;
        self.heights
            .iter_mut()
            .for_each(|height| *height = UNMEASURED);
        self.rendered.clear();
        self.recency.clear();
        self.layout_valid = false;
    }

    /// Toggle one block's expansion.
    pub fn toggle_block(&mut self, index: usize) -> bool {
        let Some(block) = self.blocks.get_mut(index) else {
            return false;
        };
        if !block.collapsible {
            return false;
        }
        block.expanded = !block.expanded;
        self.invalidate(index);
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
                self.invalidate(index);
            }
        }
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
    fn estimate_height(&self, index: usize) -> usize {
        let Some(block) = self.blocks.get(index) else {
            return 1;
        };
        let available = self.width.max(8);
        let header = 1 + display_width(&block.title) / available;
        let body_lines = if block.body.is_empty() {
            0
        } else if block.is_open() {
            // Count newlines plus a wrap allowance per line.
            let explicit = block.body.matches('\n').count() + 1;
            let wrap_allowance = block.body.len() / available.max(1);
            explicit + wrap_allowance / 2
        } else {
            COLLAPSED_BODY_LINES.min(block.body.matches('\n').count() + 1)
        };
        (header + body_lines).max(1)
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

    fn render_block(&mut self, index: usize, theme: &TuiTheme, strings: Strings) -> RenderedBlock {
        let Some(block) = self.blocks.get(index).cloned() else {
            return RenderedBlock::default();
        };
        self.stats.blocks_rendered += 1;
        render_block(&block, theme, self.width, strings)
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
        if scroll.follow {
            // Following the tail means the tail must be measured first;
            // otherwise the window is positioned against estimates for blocks
            // that were never sized.
            self.measure_tail(height, theme, strings);
        }
        self.ensure_layout();
        let total = self.offsets.last().copied().unwrap_or(0);
        let offset = if scroll.follow {
            total.saturating_sub(height)
        } else {
            scroll.offset.min(total.saturating_sub(1))
        };
        let end = (offset + height).min(total);

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

    /// Scroll offset needed to bring `index` to the top of the viewport.
    pub fn offset_of_block(&mut self, index: usize) -> usize {
        self.line_of_block(index)
    }
}

/// Fit a sequence of styled parts into `width` columns, dropping whole parts
/// from the end before truncating the last one that fits.
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
fn kind_decoration(kind: TimelineRowKind, theme: &TuiTheme) -> (&'static str, Style) {
    let unicode = theme.glyphs() == crate::theme::GlyphMode::Unicode;
    match kind {
        TimelineRowKind::UserMessage => {
            if unicode {
                ("›", theme.accent())
            } else {
                (">", theme.accent())
            }
        }
        TimelineRowKind::AgentMessage => {
            if unicode {
                ("◆", theme.base())
            } else {
                ("*", theme.base())
            }
        }
        TimelineRowKind::Reasoning => {
            if unicode {
                ("◇", theme.muted())
            } else {
                ("~", theme.muted())
            }
        }
        TimelineRowKind::Plan => (if unicode { "☰" } else { "=" }, theme.accent()),
        TimelineRowKind::ToolCall => (if unicode { "⚙" } else { "%" }, theme.muted()),
        TimelineRowKind::Command => ("$", theme.muted()),
        TimelineRowKind::FileOperation => (if unicode { "±" } else { "+" }, theme.muted()),
        TimelineRowKind::WebSearch => (if unicode { "⌕" } else { "?" }, theme.muted()),
        TimelineRowKind::TodoUpdate => (if unicode { "☑" } else { "v" }, theme.muted()),
        TimelineRowKind::Collaboration => (if unicode { "⧉" } else { "&" }, theme.accent()),
        TimelineRowKind::ImageGeneration => (if unicode { "▣" } else { "#" }, theme.muted()),
        TimelineRowKind::GitNotice => (if unicode { "⑂" } else { "g" }, theme.muted()),
        TimelineRowKind::SystemNotice => (if unicode { "•" } else { "-" }, theme.muted()),
        TimelineRowKind::PermissionRequest => (if unicode { "⚠" } else { "!" }, theme.warning()),
        TimelineRowKind::PermissionResolution => (if unicode { "✓" } else { "+" }, theme.muted()),
        TimelineRowKind::ElicitationRequest => ("?", theme.warning()),
        TimelineRowKind::ElicitationResolution => (if unicode { "✓" } else { "+" }, theme.muted()),
        TimelineRowKind::Retry => (if unicode { "↻" } else { "r" }, theme.warning()),
        TimelineRowKind::Error => (if unicode { "✗" } else { "x" }, theme.danger()),
    }
}

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
            | TimelineRowKind::UserMessage
            | TimelineRowKind::Reasoning
            | TimelineRowKind::Plan
            | TimelineRowKind::Error
            | TimelineRowKind::SystemNotice
            | TimelineRowKind::GitNotice
            | TimelineRowKind::TodoUpdate
    )
}

/// Render one block into display lines.
pub fn render_block(
    block: &Block,
    theme: &TuiTheme,
    width: usize,
    strings: Strings,
) -> RenderedBlock {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut plain: Vec<String> = Vec::new();

    let (icon, accent) = kind_decoration(block.kind, theme);
    let label = kind_label(block.kind, strings);
    let mut parts: Vec<(String, Style)> = vec![
        (format!("{icon} "), accent),
        (format!("{label} "), accent.add_modifier(Modifier::BOLD)),
        (block.title.clone(), theme.base()),
    ];
    if block.streaming {
        parts.push((" ▍".to_string(), theme.accent()));
    }
    if block.collapsible && !block.expanded {
        parts.push((
            format!("  ({})", strings.transcript_collapsed_hint()),
            theme.muted(),
        ));
    }
    let header = truncate_parts(parts, width);
    let header_plain = header
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    lines.push(Line::from(header));
    plain.push(header_plain);

    let body_width = width.saturating_sub(2).max(8);
    let body = block.body.trim_end_matches('\n');
    if !body.is_empty() {
        let indent = "  ";
        let mut rendered = if is_markdown(block.kind) {
            render_markdown(body, theme, body_width, strings)
        } else {
            render_plain(body, theme, body_width)
        };
        let style = body_style(block, theme);
        // Fold before wrapping: a collapsed block must not pay for the lines it
        // is not going to show.
        if !block.is_open() && rendered.height() > COLLAPSED_BODY_LINES {
            rendered.lines.truncate(COLLAPSED_BODY_LINES);
            rendered.plain.truncate(COLLAPSED_BODY_LINES);
            let omitted = 0;
            let _ = omitted;
        }
        for (line, text) in rendered.lines.into_iter().zip(rendered.plain) {
            let mut spans = vec![Span::raw(indent)];
            let mut styled = line;
            if !matches!(block.kind, TimelineRowKind::Error) {
                styled = styled.style(style);
            }
            spans.extend(styled.spans);
            lines.push(Line::from(spans));
            plain.push(format!("{indent}{text}"));
        }
    }

    if block.failed {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(strings.failed().to_string(), theme.danger()),
        ]));
        plain.push(format!("  {}", strings.failed()));
    }
    if block.pending_permission {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                strings.approval_waiting().to_string(),
                theme.warning().add_modifier(Modifier::BOLD),
            ),
        ]));
        plain.push(format!("  {}", strings.approval_waiting()));
    }
    if let Some(runtime) = &block.runtime_attribution {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("[{runtime}]"), theme.muted()),
        ]));
        plain.push(format!("  [{runtime}]"));
    }

    RenderedBlock {
        height: lines.len(),
        lines,
        plain,
    }
}

fn body_style(block: &Block, theme: &TuiTheme) -> Style {
    match block.kind {
        TimelineRowKind::Reasoning => theme.muted().add_modifier(Modifier::ITALIC),
        TimelineRowKind::Command | TimelineRowKind::ToolCall | TimelineRowKind::FileOperation => {
            theme.base()
        }
        TimelineRowKind::Error => theme.danger(),
        TimelineRowKind::SystemNotice | TimelineRowKind::GitNotice => theme.muted(),
        TimelineRowKind::UserMessage => theme.base().add_modifier(Modifier::BOLD),
        _ => theme.base(),
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
            title: format!("{id} title"),
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
    fn appending_a_block_does_not_invalidate_the_others() {
        let mut transcript = transcript_with(3, "hello");
        let strings = strings();
        let palette = theme();
        // Warm the cache.
        transcript.visible_lines(
            ScrollState {
                offset: 0,
                follow: false,
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
        assert!(collapsed > COLLAPSED_BODY_LINES);
        assert!(transcript.toggle_block(0));
        let expanded = {
            transcript.visible_lines(ScrollState::default(), 500, &palette, strings);
            transcript.total_height()
        };
        assert!(expanded > collapsed, "{expanded} !> {collapsed}");
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
        transcript.set_blocks(vec![entry]);
        let text = transcript.block_text(0).unwrap();
        assert!(text.contains("a title"));
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
    fn connection_blocks_render_without_a_body() {
        let mut entry = block("a", TimelineRowKind::SystemNotice, "");
        entry.collapsible = false;
        let rendered = render_block(&entry, &theme(), 60, strings());
        assert_eq!(rendered.height, 1);
    }

    #[test]
    fn cjk_bodies_respect_the_render_width() {
        let palette = theme();
        let mut transcript = Transcript::new();
        transcript.configure(40, &palette);
        transcript.set_blocks(vec![block(
            "cjk",
            TimelineRowKind::AgentMessage,
            "这是一段中文正文，用于验证按列宽换行。",
        )]);
        let lines = transcript.visible_lines(ScrollState::default(), 100, &palette, strings());
        for line in &lines {
            assert!(display_width(&line.to_string()) <= 40);
        }
    }
}

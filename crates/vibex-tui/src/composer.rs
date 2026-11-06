//! The message composer: a multi-line grapheme-correct buffer with history and
//! trigger-driven completion.
//!
//! The composer owns no product semantics. `/`, `@` and `$` are *triggers* that
//! ask the authority what they mean (`discover_agent_commands`); this module
//! only knows how to detect a trigger, present the returned candidates and
//! splice an accepted candidate back into the text.

use unicode_segmentation::UnicodeSegmentation;

use vibex_core::VibexSessionId;

use crate::text::{display_width, take_width};

/// Which trigger opened the completion menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionTrigger {
    /// `/` — composer commands and skills advertised by the authority.
    Slash,
    /// `@` — workspace files.
    At,
    /// `$` — skills.
    Dollar,
}

impl CompletionTrigger {
    pub const fn character(self) -> char {
        match self {
            Self::Slash => '/',
            Self::At => '@',
            Self::Dollar => '$',
        }
    }

    pub const fn from_character(character: char) -> Option<Self> {
        match character {
            '/' => Some(Self::Slash),
            '@' => Some(Self::At),
            '$' => Some(Self::Dollar),
            _ => None,
        }
    }
}

/// One completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Text inserted in place of the trigger word.
    pub insert: String,
    /// Short name shown in the menu.
    pub label: String,
    /// One-line explanation.
    pub detail: String,
    /// Grouping shown as a menu heading.
    pub group: String,
}

/// Which panel of an open completion drawer is showing.
///
/// A `/` drawer carries the Agent's commands and the reusable Prompts; an `@`
/// drawer carries the workspace files, the collaborators a reader may hand work
/// to, and the conversations a task may point at. `$` keeps its single Skills
/// panel, which is [`Self::Commands`]' list, so a trigger without panels still
/// has one — the same shape the desktop popup has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionTab {
    #[default]
    Commands,
    Phrases,
    Files,
    Delegation,
    Sessions,
}

impl CompletionTab {
    /// The panels a trigger opens, in header order.
    pub const fn tabs(trigger: CompletionTrigger) -> &'static [Self] {
        match trigger {
            CompletionTrigger::Slash => &[Self::Commands, Self::Phrases],
            CompletionTrigger::At => &[Self::Files, Self::Delegation, Self::Sessions],
            CompletionTrigger::Dollar => &[Self::Commands],
        }
    }

    /// Stable identifier, for a click region or a test that names a panel.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Commands => "commands",
            Self::Phrases => "phrases",
            Self::Files => "files",
            Self::Delegation => "delegation",
            Self::Sessions => "sessions",
        }
    }

    /// The panel one `Left` (`forward == false`) or `Right` press moves to.
    ///
    /// A trigger's panels clamp instead of wrapping, so a press that cannot
    /// change the panel answers with the one already showing: the caller reads
    /// that as "the key was not mine" and lets the caret keep its usual
    /// movement, exactly as the desktop composer does.
    pub fn moved(self, tabs: &[Self], forward: bool) -> Self {
        let Some(index) = tabs.iter().position(|tab| *tab == self) else {
            return tabs.first().copied().unwrap_or(self);
        };
        let next = if forward {
            tabs.get(index + 1)
        } else {
            index.checked_sub(1).and_then(|index| tabs.get(index))
        };
        next.copied().unwrap_or(self)
    }
}

/// The candidate lists one discovery answer carries, one per panel.
///
/// They are kept apart rather than flattened so a switch is local: the answer
/// already holds every list the trigger opens, which is what keeps `←`/`→` from
/// returning to the authority, and what lets each panel be drawn on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionLists {
    pub commands: Vec<Completion>,
    pub phrases: Vec<Completion>,
    pub files: Vec<Completion>,
    pub delegation: Vec<Completion>,
    pub sessions: Vec<Completion>,
}

impl CompletionLists {
    /// The rows one panel holds.
    pub fn rows(&self, tab: CompletionTab) -> &[Completion] {
        match tab {
            CompletionTab::Commands => &self.commands,
            CompletionTab::Phrases => &self.phrases,
            CompletionTab::Files => &self.files,
            CompletionTab::Delegation => &self.delegation,
            CompletionTab::Sessions => &self.sessions,
        }
    }
}

/// The open completion menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionMenu {
    pub trigger: CompletionTrigger,
    /// Byte range of the trigger word in the buffer.
    pub start: usize,
    /// What has been typed after the trigger.
    ///
    /// The candidates come from the authority, which matched them against the
    /// query the request carried; the reader keeps typing while that answer
    /// travels, so this is what the drawer filters the rows by in the meantime.
    pub query: String,
    /// Every list the trigger's panels hold.
    pub lists: CompletionLists,
    /// Which panel is showing. Clamped into the trigger's own panels by
    /// [`Self::active_tab`], so a leftover choice cannot survive a new trigger.
    pub tab: CompletionTab,
    pub selected: usize,
    pub loading: bool,
}

/// How many rows the drawer shows before it scrolls.
pub const MAX_VISIBLE_COMPLETIONS: usize = 8;

impl CompletionMenu {
    /// The panels this trigger opens, in header order.
    pub const fn tabs(&self) -> &'static [CompletionTab] {
        CompletionTab::tabs(self.trigger)
    }

    /// Whether the drawer has more than one panel to switch between.
    ///
    /// `$` opens a single Skills panel and keeps the plain caret movement its
    /// arrow keys always had; `/` and `@` are the triggers whose panels are
    /// worth a header row and a pair of keys.
    pub fn tabs_available(&self) -> bool {
        self.tabs().len() > 1
    }

    /// The panel showing, clamped into the trigger's own panels.
    pub fn active_tab(&self) -> CompletionTab {
        let tabs = self.tabs();
        if tabs.contains(&self.tab) {
            self.tab
        } else {
            tabs.first().copied().unwrap_or_default()
        }
    }

    /// The rows of the panel showing.
    pub fn items(&self) -> &[Completion] {
        self.lists.rows(self.active_tab())
    }

    /// Switch to the next (`forward`) or previous panel.
    ///
    /// Returns whether the press changed anything. At the edge in that
    /// direction the key is *not* consumed: the caret keeps its usual movement
    /// inside the `/query` being edited, which is also what keeps the drawer
    /// open.
    pub fn step_tab(&mut self, forward: bool) -> bool {
        if !self.tabs_available() {
            return false;
        }
        let next = self.active_tab().moved(self.tabs(), forward);
        self.select_tab(next)
    }

    /// Switch to `tab`, returning whether the panel changed.
    ///
    /// The header's click and its keys run the same switch, so a panel reached
    /// by pointer restarts the selection exactly as one reached by `←`/`→`: the
    /// panel is a fresh list, and an index kept from the one just left would
    /// highlight a row the reader never chose.
    pub fn select_tab(&mut self, tab: CompletionTab) -> bool {
        if !self.tabs_available() || !self.tabs().contains(&tab) || self.active_tab() == tab {
            return false;
        }
        self.tab = tab;
        self.selected = self.filtered(&self.query).first().copied().unwrap_or(0);
        true
    }

    /// The indices of the candidates the query matches.
    pub fn filtered(&self, query: &str) -> Vec<usize> {
        let query = query.to_lowercase();
        self.items()
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                query.is_empty()
                    || item.label.to_lowercase().contains(&query)
                    || item.detail.to_lowercase().contains(&query)
                    // The insertion carries what the label leaves out — a file's
                    // directory, a command's arguments — and it is what the
                    // authority matched a query against, so a row it returned
                    // for a path would otherwise disappear the moment the
                    // reader typed the rest of that path.
                    || item.insert.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// The rows to draw: the matches, windowed around the selection.
    ///
    /// The menu carries the query the drawer is drawn for, so the window is
    /// placed in the *filtered* list rather than in the whole one: the selected
    /// row is always on screen even after the list has scrolled.
    pub fn visible(&self) -> Vec<usize> {
        let matches = self.filtered(&self.query);
        if matches.len() <= MAX_VISIBLE_COMPLETIONS {
            return matches;
        }
        let half = MAX_VISIBLE_COMPLETIONS / 2;
        let position = matches
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let start = position
            .saturating_sub(half)
            .min(matches.len() - MAX_VISIBLE_COMPLETIONS);
        matches[start..start + MAX_VISIBLE_COMPLETIONS].to_vec()
    }

    /// The candidate the drawer is offering, when it is offering one.
    ///
    /// The selection is an index into the whole list while the reader only ever
    /// sees the rows their query matches. An index the query has since filtered
    /// out therefore has nothing behind it, and `Enter` has to be able to tell
    /// the difference before it takes a row rather than sends the draft.
    pub fn selected_item(&self) -> Option<&Completion> {
        self.filtered(&self.query)
            .contains(&self.selected)
            .then(|| self.items().get(self.selected))
            .flatten()
    }

    /// Take a new query, keeping the selection on a row it still matches.
    ///
    /// The reader's cursor moves the selection across rows that are no longer
    /// on screen otherwise: the highlighted row would be one the drawer does
    /// not draw, and `Enter` would insert a command they cannot see.
    pub fn reseat(&mut self, query: String) {
        self.query = query;
        self.reseat_selection();
    }

    /// Point the selection at the first row the query still matches.
    fn reseat_selection(&mut self) {
        let matches = self.filtered(&self.query);
        if !matches.contains(&self.selected) {
            self.selected = matches.first().copied().unwrap_or(0);
        }
    }
}

/// How many undo snapshots the buffer keeps.
///
/// A draft is a paragraph or two, not a document; a hundred steps is far more
/// history than anyone reaches for, and the snapshots are two small fields
/// each.
pub const MAX_UNDO: usize = 100;

/// What the last mutation was, so consecutive typing collapses into one step.
///
/// Undo that walks back one character at a time is worse than no undo: the
/// reader wants the sentence they just typed gone, not letters. Batches break
/// where the text changes kind — a space ends a word — which is the boundary a
/// person would draw too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    InsertWord,
    InsertSpace,
    /// A paste, a kill, a yank or a programmatic replace: always its own step.
    Block,
    Delete,
}

impl EditKind {
    /// Whether a repeat of this kind continues the previous step.
    const fn coalesces(self) -> bool {
        matches!(
            self,
            EditKind::InsertWord | EditKind::InsertSpace | EditKind::Delete
        )
    }
}

/// One point the buffer can be returned to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    text: String,
    cursor: usize,
    chips: Vec<Chip>,
    selection: Option<DraftSelection>,
}

/// A selection inside the draft, in byte offsets.
///
/// The `head` is always the cursor: every motion moves the head, and a
/// selection is only non-empty while the two ends differ. Keeping the two in
/// step means a selection never has to be translated when the text around it
/// changes, because the cursor's own maintenance already did that work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftSelection {
    /// The end that stays put while the head moves.
    pub anchor: usize,
    /// The moving end, which is also where the cursor is.
    pub head: usize,
}

/// One visual row of the draft: what to paint and where it came from.
///
/// A row's `source_start` is the byte offset in [`ComposerBuffer::text`] of its
/// first character, which is what lets a selection (held in byte offsets) be
/// painted on wrapped display rows without a second copy of the wrap maths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayRow {
    pub text: String,
    /// Whether the cursor is on this row's logical line.
    pub cursor_line: bool,
    /// Byte offset in the buffer of the row's first character.
    pub source_start: usize,
}

/// A pasted block or an attached image, collapsed into a label until the
/// message is sent.
///
/// The buffer's text holds the label (`[Pasted: 42 lines]`, `[Image #1]`), so
/// everything that measures, wraps or moves the cursor keeps working
/// unchanged; what the label stands for rides alongside. A chip is atomic: the
/// cursor steps over it and one `Backspace` removes all of it, because a
/// hundred-line paste must not be a hundred keys to undo.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Chip {
    /// Byte range of the label inside the buffer text.
    start: usize,
    end: usize,
    label: String,
    payload: ChipPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChipPayload {
    /// A collapsed paste: the bytes the label stands for, put back verbatim on
    /// the way out.
    Paste(String),
    /// An image the prompt carries as an attachment.
    Image(ImageAttachment),
}

/// A chip the caret is standing on, for the panel that explains it.
///
/// A label is all the draft draws — `[Pasted: 40 lines]` is one row wide
/// whatever it stands for — so the reader has no way to check what a chip
/// holds without opening it. This is the read-only half of that: what the
/// label stands for, and whether the caret is *on* the chip rather than just
/// after it, which is the difference between `Enter` opening the chip and
/// `Enter` sending the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChipPeek<'a> {
    /// The label as the draft draws it.
    pub label: &'a str,
    /// What the label stands for.
    pub payload: ChipPeekPayload<'a>,
    /// Whether the caret is inside the label rather than at its right edge.
    pub on_chip: bool,
}

/// What a chip holds, borrowed from the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipPeekPayload<'a> {
    /// A collapsed paste: the bytes the label stands for.
    Paste(&'a str),
    /// An attached image, drawn as a raster when the terminal can show one.
    Image(&'a ImageAttachment),
}

impl ChipPeek<'_> {
    /// Whether this chip has text to expand into the draft.
    pub const fn is_paste(&self) -> bool {
        matches!(self.payload, ChipPeekPayload::Paste(_))
    }
}

/// An image attached to the draft, before it is turned into a wire attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAttachment {
    /// `Image #1`, exactly as the label reads.
    pub label: String,
    pub mime_type: String,
    pub source: ImageSource,
    /// How many bytes the picture is, measured when it was attached.
    ///
    /// Named under the picture in the preview, and measured there rather than
    /// read there: a file's size is a stat, and the preview is drawn on every
    /// frame the caret rests on the chip.
    pub byte_len: usize,
    /// The picture reduced to something a character grid can draw.
    ///
    /// Decoded once, when the image is attached. `None` for bytes that are not
    /// a picture this client can read, which is a preview that says so rather
    /// than a paste that fails.
    pub thumbnail: Option<ImageThumbnail>,
}

/// A picture as a block of terminal cells.
///
/// Each cell carries two pixels — the half-block character is the upper one,
/// the cell's background the lower — so a raster is `cols` wide and `2 * rows`
/// pixels tall, which is the closest a character grid comes to square pixels.
/// The pixels are kept as plain RGB rather than a `ratatui::Color` so the data
/// belongs to the attachment rather than to the terminal it will be drawn on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageThumbnail {
    /// The picture's own width and height, before it was scaled.
    pub width: u32,
    pub height: u32,
    /// How many cells the raster is, left to right and top to bottom.
    pub cols: u16,
    pub rows: u16,
    /// `cols * rows` cells, row-major, each one `[top r, g, b, bottom r, g, b]`.
    pub cells: Vec<[u8; 6]>,
}

impl ImageThumbnail {
    /// The two pixels of one cell, or `None` outside the raster.
    pub fn cell(&self, column: u16, row: u16) -> Option<([u8; 3], [u8; 3])> {
        if column >= self.cols || row >= self.rows {
            return None;
        }
        let cell = self
            .cells
            .get(usize::from(row) * usize::from(self.cols) + usize::from(column))?;
        Some(([cell[0], cell[1], cell[2]], [cell[3], cell[4], cell[5]]))
    }
}

/// A picture on its way into the draft: everything `insert_image` needs that
/// the label and the number it assigns do not already carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedImage {
    pub mime_type: String,
    pub source: ImageSource,
    /// The bytes' length. See [`ImageAttachment::byte_len`].
    pub byte_len: usize,
    /// The decoded raster, when the bytes are a picture. See
    /// [`ImageAttachment::thumbnail`].
    pub thumbnail: Option<ImageThumbnail>,
}

/// Where an attached image's bytes live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// A file the runtime can read itself.
    Path(String),
    /// Bytes read from the clipboard modelessly. Shared so that the undo
    /// history can hold a snapshot without copying the pixels.
    Bytes(std::sync::Arc<Vec<u8>>),
}

/// The line count at which a paste collapses into a chip.
pub const PASTE_CHIP_LINES: usize = 4;
/// The byte size at which a paste collapses into a chip however few lines it
/// has. A single-line minified log is still not something to put in a prompt.
pub const PASTE_CHIP_BYTES: usize = 10_000;
/// How many images one prompt may carry.
///
/// The wire accepts more, but an Agent's context does not: a dozen screenshots
/// is a turn nobody asked for, and the reader cannot see them all in the
/// composer.
pub const IMAGE_CAP: usize = 10;
/// The largest image the client will attach, matching the ACP adapter's own
/// limit so the reader is told before the Agent silently degrades the block.
pub const IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;

/// A multi-line editable buffer with a grapheme-aligned cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerBuffer {
    text: String,
    /// Cursor position as a byte offset, always on a grapheme boundary.
    cursor: usize,
    /// Desired column, preserved while moving vertically through short lines.
    preferred_column: Option<usize>,
    /// Undo history, oldest first, with `history_index` pointing at the live
    /// state.
    history: Vec<Snapshot>,
    history_index: usize,
    last_edit: Option<EditKind>,
    /// The last killed text, put back by `Ctrl+Y`.
    kill_buffer: String,
    /// Collapsed pastes and attached images, in buffer order.
    chips: Vec<Chip>,
    /// The number the next attached image gets. Monotonic for the draft, so a
    /// label never refers to two different pictures.
    next_image_number: u32,
    /// The draft selection, while one is being made.
    selection: Option<DraftSelection>,
    /// The width the last `display_lines` call used, so a click can map a
    /// screen row back to a wrapped row without the renderer passing it in.
    last_display_width: usize,
}

impl Default for ComposerBuffer {
    fn default() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            preferred_column: None,
            history: vec![Snapshot {
                text: String::new(),
                cursor: 0,
                chips: Vec::new(),
                selection: None,
            }],
            history_index: 0,
            last_edit: None,
            kill_buffer: String::new(),
            chips: Vec::new(),
            next_image_number: 1,
            selection: None,
            last_display_width: 80,
        }
    }
}

impl ComposerBuffer {
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            history: vec![Snapshot {
                text: text.clone(),
                cursor,
                chips: Vec::new(),
                selection: None,
            }],
            text,
            cursor,
            preferred_column: None,
            history_index: 0,
            last_edit: None,
            kill_buffer: String::new(),
            chips: Vec::new(),
            next_image_number: 1,
            selection: None,
            last_display_width: 80,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// The last killed text, for tests and for a footer that wants to say so.
    pub fn kill_buffer(&self) -> &str {
        &self.kill_buffer
    }

    /// The selection as a byte range in reading order, when it covers anything.
    ///
    /// The range is widened to whole chips: a chip is one object, so a
    /// selection that touches one selects all of it. Otherwise half a
    /// `[Pasted: 42 lines]` label could be copied and the other half left
    /// behind.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let selection = self.selection?;
        if selection.anchor == selection.head {
            return None;
        }
        let (mut start, mut end) = if selection.anchor <= selection.head {
            (selection.anchor, selection.head)
        } else {
            (selection.head, selection.anchor)
        };
        // Widen until stable: covering one chip's label can reach into the
        // next, and a range that grew must be checked again.
        let mut changed = true;
        while changed {
            changed = false;
            for chip in &self.chips {
                if chip.start < end && start < chip.end {
                    if chip.start < start {
                        start = chip.start;
                        changed = true;
                    }
                    if chip.end > end {
                        end = chip.end;
                        changed = true;
                    }
                }
            }
        }
        (start < end).then_some((start, end))
    }

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    /// The selected draft text, with chip labels exactly as they are drawn.
    pub fn selected_text(&self) -> Option<String> {
        self.selection()
            .map(|(start, end)| self.text[start..end].to_string())
    }

    /// The selected text with chip labels expanded back to what they stand for.
    ///
    /// Cutting or copying a collapsed paste must not put `[Pasted: 42 lines]`
    /// on the clipboard where the reader expected the lines themselves.
    fn selected_expanded_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        let mut out = String::new();
        let mut cursor = start;
        for chip in &self.chips {
            if chip.start < start || chip.end > end {
                continue;
            }
            out.push_str(&self.text[cursor..chip.start]);
            match &chip.payload {
                ChipPayload::Paste(content) => out.push_str(content),
                // What goes to the clipboard should read like the draft, and
                // the draft shows the label.
                ChipPayload::Image(_) => out.push_str(&chip.label),
            }
            cursor = chip.end;
        }
        out.push_str(&self.text[cursor..end]);
        Some(out)
    }

    /// Drop the highlight. Returns whether there was one.
    pub fn clear_selection(&mut self) -> bool {
        let had = self.has_selection();
        self.selection = None;
        had
    }

    pub fn select_all(&mut self) {
        if self.text.is_empty() {
            self.selection = None;
            return;
        }
        self.cursor = self.text.len();
        self.preferred_column = None;
        self.selection = Some(DraftSelection {
            anchor: 0,
            head: self.text.len(),
        });
        self.break_batch();
    }

    /// Start a selection at the cursor. A click leaves it empty and harmless;
    /// a drag extends it.
    pub fn begin_selection(&mut self) {
        self.selection = Some(DraftSelection {
            anchor: self.cursor,
            head: self.cursor,
        });
    }

    /// Extend the selection, keeping the anchor where it was.
    ///
    /// Every `extend_*` is a plain motion plus the anchor: the motions already
    /// know how to step over chips and graphemes, and duplicating that here
    /// would be two implementations of the same movement.
    fn extend_with(&mut self, motion: impl FnOnce(&mut Self)) {
        let anchor = self
            .selection
            .map(|selection| selection.anchor)
            .unwrap_or(self.cursor);
        motion(self);
        self.selection = Some(DraftSelection {
            anchor,
            head: self.cursor,
        });
        self.break_batch();
    }

    pub fn extend_left(&mut self) {
        self.extend_with(|buffer| buffer.move_left());
    }

    pub fn extend_right(&mut self) {
        self.extend_with(|buffer| buffer.move_right());
    }

    pub fn extend_word_left(&mut self) {
        self.extend_with(|buffer| buffer.move_word_left());
    }

    pub fn extend_word_right(&mut self) {
        self.extend_with(|buffer| buffer.move_word_right());
    }

    pub fn extend_up(&mut self) {
        self.extend_with(|buffer| buffer.move_up());
    }

    pub fn extend_down(&mut self) {
        self.extend_with(|buffer| buffer.move_down());
    }

    pub fn extend_line_start(&mut self) {
        self.extend_with(|buffer| buffer.move_line_start());
    }

    pub fn extend_line_end(&mut self) {
        self.extend_with(|buffer| buffer.move_line_end());
    }

    pub fn extend_to_start(&mut self) {
        self.extend_with(|buffer| buffer.move_to_start());
    }

    pub fn extend_to_end(&mut self) {
        self.extend_with(|buffer| buffer.move_to_end());
    }

    /// Extend the selection to a display cell, for a drag in the composer.
    pub fn extend_selection_to_cell(&mut self, row: u16, column: u16) {
        self.extend_with(|buffer| buffer.move_cursor_to_cell(row, column));
    }

    /// Delete the selected range, if there is one, leaving the cursor at its
    /// start. The caller records the edit.
    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            self.selection = None;
            return false;
        };
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.preferred_column = None;
        self.selection = None;
        self.reshape_chips(start, end);
        self.shift_chips(start, -((end - start) as isize));
        true
    }

    /// Byte ranges of every chip label, in buffer order.
    pub fn chip_ranges(&self) -> Vec<(usize, usize)> {
        self.chips
            .iter()
            .map(|chip| (chip.start, chip.end))
            .collect()
    }

    /// The chip the caret is on, or the one whose right edge it stands at.
    ///
    /// Both positions count: [`Self::insert_paste`] and [`Self::insert_image`]
    /// leave the caret immediately after the label they inserted, and that is
    /// where the reader is when the question "what did I just paste?" is worth
    /// answering. Only the first is a position `Enter` can act from, which is
    /// why [`ChipPeek::on_chip`] travels with it.
    pub fn chip_peek(&self) -> Option<ChipPeek<'_>> {
        let index = self.chip_covering(self.cursor).or_else(|| {
            self.chips
                .iter()
                .position(|chip| chip.end == self.cursor && chip.start < chip.end)
        })?;
        let chip = self.chips.get(index)?;
        Some(ChipPeek {
            label: &chip.label,
            payload: match &chip.payload {
                ChipPayload::Paste(content) => ChipPeekPayload::Paste(content),
                ChipPayload::Image(image) => ChipPeekPayload::Image(image),
            },
            on_chip: chip.start <= self.cursor && self.cursor < chip.end,
        })
    }

    /// Replace the paste chip under the caret with its content, in place.
    ///
    /// Strictly on-chip: a caret at the chip's right edge is where the reader
    /// finished a sentence that happens to end in a paste, and `Enter` there
    /// means "send", not "expand". Returns whether a paste was opened.
    pub fn expand_paste_at_cursor(&mut self) -> bool {
        let Some(index) = self.chip_covering(self.cursor) else {
            return false;
        };
        if !matches!(self.chips[index].payload, ChipPayload::Paste(_)) {
            return false;
        }
        self.expand_chip(index)
    }

    pub fn can_undo(&self) -> bool {
        self.history_index > 0
    }

    pub fn can_redo(&self) -> bool {
        self.history_index + 1 < self.history.len()
    }

    /// Record the state after a mutation.
    ///
    /// Called *after* the text changes: a coalescing repeat replaces the live
    /// snapshot, anything else becomes a new step and drops the redo tail.
    fn record(&mut self, kind: EditKind) {
        let snapshot = Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            chips: self.chips.clone(),
            selection: self.selection,
        };
        let coalesce = self.last_edit == Some(kind) && kind.coalesces();
        if coalesce && self.history_index < self.history.len() {
            self.history[self.history_index] = snapshot;
        } else {
            self.history.truncate(self.history_index + 1);
            self.history.push(snapshot);
            self.history_index = self.history.len() - 1;
            while self.history.len() > MAX_UNDO {
                self.history.remove(0);
                self.history_index = self.history_index.saturating_sub(1);
            }
        }
        self.last_edit = Some(kind);
    }

    /// Break the typing batch, so the next edit starts a new undo step.
    ///
    /// Every cursor move calls this: undoing across a jump would move the text
    /// out from under a cursor the reader deliberately placed.
    fn break_batch(&mut self) {
        self.last_edit = None;
    }

    pub fn undo(&mut self) -> bool {
        if !self.can_undo() {
            return false;
        }
        self.history_index -= 1;
        self.restore();
        true
    }

    pub fn redo(&mut self) -> bool {
        if !self.can_redo() {
            return false;
        }
        self.history_index += 1;
        self.restore();
        true
    }

    fn restore(&mut self) {
        let snapshot = self.history[self.history_index].clone();
        self.text = snapshot.text;
        self.chips = snapshot.chips;
        self.selection = snapshot.selection;
        self.cursor = snapshot.cursor.min(self.text.len());
        self.preferred_column = None;
        self.break_batch();
    }

    /// A paste, collapsed to a chip when it is big enough to bury the draft.
    ///
    /// Returns whether it became a chip. Small pastes are inserted literally:
    /// a chip for two lines is more chrome than content.
    pub fn insert_paste(&mut self, value: &str) -> bool {
        let value = normalize_line_breaks(value);
        self.delete_selection();
        // Re-pasting the content of a chip the cursor is on expands it instead
        // of adding a second copy of the same thing.
        if let Some(index) = self.chip_covering(self.cursor).or_else(|| {
            self.chips
                .iter()
                .position(|chip| chip.end == self.cursor || chip.start == self.cursor)
        }) && self.chips[index].payload == ChipPayload::Paste(value.clone())
        {
            // The paste is already here, collapsed: show it rather than
            // inserting a second copy of the same bytes.
            self.expand_chip(index);
            return false;
        }
        let lines = value.lines().count().max(1);
        if lines < PASTE_CHIP_LINES && value.len() <= PASTE_CHIP_BYTES {
            self.insert_str(&value);
            return false;
        }
        let label = paste_label(&value, lines);
        let start = self.cursor;
        self.text.insert_str(start, &label);
        let end = start + label.len();
        self.cursor = end;
        self.preferred_column = None;
        self.chips.push(Chip {
            start,
            end,
            label,
            payload: ChipPayload::Paste(value),
        });
        self.chips.sort_by_key(|chip| chip.start);
        self.record(EditKind::Block);
        true
    }

    /// Replace one paste chip's label with its content, in place.
    ///
    /// An image chip has no text to expand into, so it declines.
    fn expand_chip(&mut self, index: usize) -> bool {
        let Some(chip) = self.chips.get(index).cloned() else {
            return false;
        };
        let ChipPayload::Paste(content) = chip.payload else {
            return false;
        };
        self.text.replace_range(chip.start..chip.end, &content);
        self.chips.remove(index);
        self.cursor = chip.start + content.len();
        let delta = content.len() as isize - (chip.end - chip.start) as isize;
        self.shift_chips(chip.end, delta);
        self.preferred_column = None;
        self.record(EditKind::Block);
        true
    }

    /// The text to send: every chip replaced by what was actually pasted.
    pub fn expanded_text(&self) -> String {
        self.outgoing().text
    }

    /// The message as it will be sent, with every image's place in it.
    ///
    /// A label is dropped from the text — the Agent is not told about a
    /// placeholder it cannot see — so the place the label occupied has to travel
    /// beside it, as an offset the other clients can put the picture back at.
    /// The wire counts that offset in UTF-16 units, and it counts it in the text
    /// *after* this expansion, which is why the offset is taken here rather than
    /// from the draft: a paste expanded in front of an image moves it.
    pub fn outgoing(&self) -> Outgoing {
        let mut text = String::with_capacity(self.text.len());
        let mut images = Vec::new();
        let mut cursor = 0usize;
        for chip in &self.chips {
            if chip.start < cursor || chip.end > self.text.len() {
                continue;
            }
            text.push_str(&self.text[cursor..chip.start]);
            match &chip.payload {
                ChipPayload::Paste(content) => text.push_str(content),
                ChipPayload::Image(image) => images.push((
                    image.clone(),
                    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX),
                )),
            }
            cursor = chip.end;
        }
        text.push_str(&self.text[cursor..]);
        Outgoing { text, images }
    }

    /// The images the draft carries, in the order they were attached.
    pub fn images(&self) -> Vec<ImageAttachment> {
        self.chips
            .iter()
            .filter_map(|chip| match &chip.payload {
                ChipPayload::Image(image) => Some(image.clone()),
                ChipPayload::Paste(_) => None,
            })
            .collect()
    }

    pub fn image_count(&self) -> usize {
        self.chips
            .iter()
            .filter(|chip| matches!(chip.payload, ChipPayload::Image(_)))
            .count()
    }

    /// Attach an image at the cursor, as `[Image #N]` and a trailing space.
    ///
    /// Returns the label it was given, or `None` when the draft already holds
    /// as many images as a prompt should carry.
    pub fn insert_image(&mut self, image: AttachedImage) -> Option<String> {
        if self.image_count() >= IMAGE_CAP {
            return None;
        }
        self.delete_selection();
        let number = self.next_image_number;
        self.next_image_number += 1;
        let label = format!("[Image #{number}]");
        let start = self.cursor;
        self.text.insert_str(start, &label);
        let end = start + label.len();
        self.cursor = end;
        self.preferred_column = None;
        self.reshape_chips(start, start);
        self.shift_chips(start, label.len() as isize);
        self.chips.push(Chip {
            start,
            end,
            label: label.clone(),
            payload: ChipPayload::Image(ImageAttachment {
                label: label.clone(),
                mime_type: image.mime_type,
                source: image.source,
                byte_len: image.byte_len,
                thumbnail: image.thumbnail,
            }),
        });
        self.chips.sort_by_key(|chip| chip.start);
        self.record(EditKind::Block);
        Some(label)
    }

    /// Load a draft that was taken away earlier: its text, then the images it
    /// carried, put back at the end.
    ///
    /// The labels are regenerated rather than remembered: a label is only a
    /// number, and what matters is that the picture reaches the Agent.
    pub fn set_draft(&mut self, text: impl Into<String>, images: Vec<(ImageAttachment, u32)>) {
        self.set_text(text);
        // The offsets are the wire's — UTF-16 units into this same text — so a
        // message pulled back out of the queue gets its pictures where they
        // were, not stacked at the end of the paragraph. Each label inserted in
        // front of a later one moves it, so they are placed in order and the
        // shift is carried.
        let mut images = images;
        images.sort_by_key(|(_, offset)| *offset);
        let mut shift = 0usize;
        for (image, offset) in images {
            let byte = utf16_offset_to_byte(self.text(), offset as usize) + shift;
            self.cursor = byte.min(self.text.len());
            let attached = AttachedImage {
                mime_type: image.mime_type,
                source: image.source,
                byte_len: image.byte_len,
                thumbnail: image.thumbnail,
            };
            if let Some(label) = self.insert_image(attached) {
                shift += label.len();
            }
        }
    }

    /// Take the text and the images out together, leaving an empty buffer.
    ///
    /// The two travel as one value because the message is one thing: a caller
    /// that took the text and then asked for the images could lose them to an
    /// intervening edit.
    pub fn take_with_attachments(&mut self) -> (String, Vec<ImageAttachment>) {
        let outgoing = self.take_outgoing();
        (
            outgoing.text,
            outgoing
                .images
                .into_iter()
                .map(|(image, _)| image)
                .collect(),
        )
    }

    /// Take the outgoing message out, leaving an empty buffer.
    pub fn take_outgoing(&mut self) -> Outgoing {
        let outgoing = self.outgoing();
        self.clear_taken();
        outgoing
    }

    /// Take the expanded text out, leaving an empty buffer.
    pub fn take_expanded(&mut self) -> String {
        let outgoing = self.take_outgoing();
        outgoing.text
    }

    fn clear_taken(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.preferred_column = None;
        self.selection = None;
        self.chips.clear();
        self.next_image_number = 1;
        self.record(EditKind::Block);
    }

    /// How many pastes are currently collapsed.
    pub fn chip_count(&self) -> usize {
        self.chips.len()
    }

    fn chip_covering(&self, index: usize) -> Option<usize> {
        self.chips
            .iter()
            .position(|chip| index >= chip.start && index < chip.end)
    }

    /// Drop any chip the edit range touched, and move the rest with the text.
    ///
    /// A chip is only valid while its label is exactly where it was put; an
    /// edit that reaches into one dissolves it into literal text rather than
    /// leaving a range pointing at the wrong bytes.
    fn reshape_chips(&mut self, from: usize, to: usize) {
        self.chips
            .retain(|chip| chip.end <= from || chip.start >= to);
    }

    fn shift_chips(&mut self, at: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        for chip in &mut self.chips {
            if chip.start >= at {
                chip.start = chip.start.saturating_add_signed(delta);
                chip.end = chip.end.saturating_add_signed(delta);
            }
        }
    }

    /// Remove a whole chip, label and all.
    fn delete_chip_at(&mut self, index: usize) -> bool {
        let Some(chip) = self.chips.get(index).cloned() else {
            return false;
        };
        self.text.replace_range(chip.start..chip.end, "");
        self.chips.remove(index);
        self.shift_chips(chip.end, -((chip.end - chip.start) as isize));
        self.cursor = chip.start;
        self.preferred_column = None;
        self.record(EditKind::Block);
        true
    }

    pub fn clear(&mut self) {
        if self.text.is_empty() {
            self.selection = None;
            return;
        }
        self.text.clear();
        self.cursor = 0;
        self.preferred_column = None;
        self.selection = None;
        self.chips.clear();
        self.next_image_number = 1;
        self.record(EditKind::Block);
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.preferred_column = None;
        self.selection = None;
        self.chips.clear();
        self.next_image_number = 1;
        self.record(EditKind::Block);
    }

    /// Take the text out, leaving an empty buffer.
    pub fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.preferred_column = None;
        self.selection = None;
        self.chips.clear();
        self.next_image_number = 1;
        self.record(EditKind::Block);
        text
    }

    pub fn insert_char(&mut self, character: char) {
        // Typing over a selection replaces it, which is the whole point of
        // being able to select in a draft.
        self.delete_selection();
        let at = self.cursor;
        self.text.insert(at, character);
        self.cursor += character.len_utf8();
        self.preferred_column = None;
        self.reshape_chips(at, at);
        self.shift_chips(at, character.len_utf8() as isize);
        let kind = if character.is_whitespace() {
            EditKind::InsertSpace
        } else {
            EditKind::InsertWord
        };
        self.record(kind);
    }

    pub fn insert_str(&mut self, value: &str) {
        self.delete_selection();
        let at = self.cursor;
        self.text.insert_str(at, value);
        self.cursor += value.len();
        self.preferred_column = None;
        self.reshape_chips(at, at);
        self.shift_chips(at, value.len() as isize);
        self.record(EditKind::Block);
    }

    /// Delete the grapheme before the cursor.
    pub fn backspace(&mut self) -> bool {
        if self.delete_selection() {
            self.record(EditKind::Delete);
            return true;
        }
        if self.cursor == 0 {
            return false;
        }
        // A chip is one object: the whole paste goes with one press.
        if let Some(index) = self.chips.iter().position(|chip| chip.end == self.cursor) {
            return self.delete_chip_at(index);
        }
        let start = self.previous_grapheme_boundary();
        let end = self.cursor;
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.preferred_column = None;
        self.reshape_chips(start, end);
        self.shift_chips(start, -((end - start) as isize));
        self.record(EditKind::Delete);
        true
    }

    /// Delete the grapheme under the cursor.
    pub fn delete(&mut self) -> bool {
        if self.delete_selection() {
            self.record(EditKind::Delete);
            return true;
        }
        if self.cursor >= self.text.len() {
            return false;
        }
        if let Some(index) = self.chips.iter().position(|chip| chip.start == self.cursor) {
            return self.delete_chip_at(index);
        }
        let start = self.cursor;
        let end = self.next_grapheme_boundary();
        self.text.replace_range(start..end, "");
        self.preferred_column = None;
        self.reshape_chips(start, end);
        self.shift_chips(start, -((end - start) as isize));
        self.record(EditKind::Delete);
        true
    }

    /// Delete the word before the cursor, the way a shell's `Ctrl+W` does.
    ///
    /// Whitespace-delimited, not class-delimited: `Ctrl+W` in a shell removes
    /// `src/net.rs` in one press, and a composer that stopped at the dot would
    /// be behaving like an editor instead.
    pub fn delete_word_before(&mut self) -> bool {
        if let Some(selected) = self.selected_expanded_text()
            && self.delete_selection()
        {
            self.kill_buffer = selected;
            self.record(EditKind::Block);
            return true;
        }
        let start = self.whitespace_word_start();
        if start == self.cursor {
            return false;
        }
        self.kill_buffer = self.text[start..self.cursor].to_string();
        let end = self.cursor;
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.preferred_column = None;
        self.reshape_chips(start, end);
        self.shift_chips(start, -((end - start) as isize));
        self.record(EditKind::Block);
        true
    }

    /// Delete the word after the cursor, for `Alt+D`.
    pub fn delete_word_after(&mut self) -> bool {
        if let Some(selected) = self.selected_expanded_text()
            && self.delete_selection()
        {
            self.kill_buffer = selected;
            self.record(EditKind::Block);
            return true;
        }
        let end = self.word_end(self.cursor);
        if end == self.cursor {
            return false;
        }
        self.kill_buffer = self.text[self.cursor..end].to_string();
        let start = self.cursor;
        self.text.replace_range(start..end, "");
        self.preferred_column = None;
        self.reshape_chips(start, end);
        self.shift_chips(start, -((end - start) as isize));
        self.record(EditKind::Block);
        true
    }

    /// Delete from the cursor back to the start of the word, for
    /// `Alt+Backspace`.
    pub fn delete_word_backward(&mut self) -> bool {
        if let Some(selected) = self.selected_expanded_text()
            && self.delete_selection()
        {
            self.kill_buffer = selected;
            self.record(EditKind::Block);
            return true;
        }
        let start = self.word_start(self.cursor);
        if start == self.cursor {
            return false;
        }
        self.kill_buffer = self.text[start..self.cursor].to_string();
        let end = self.cursor;
        self.text.replace_range(start..end, "");
        self.cursor = start;
        self.preferred_column = None;
        self.reshape_chips(start, end);
        self.shift_chips(start, -((end - start) as isize));
        self.record(EditKind::Block);
        true
    }

    /// Kill from the cursor to the end of the line, for `Ctrl+K`.
    pub fn kill_to_line_end(&mut self) -> bool {
        if let Some(selected) = self.selected_expanded_text()
            && self.delete_selection()
        {
            self.kill_buffer = selected;
            self.record(EditKind::Block);
            return true;
        }
        let (_, line_end) = self.cursor_line();
        // At the end of a line the newline itself is the next thing to go, so
        // repeated `Ctrl+K` joins lines the way a reader expects.
        let end = if self.cursor == line_end {
            self.next_grapheme_boundary()
        } else {
            line_end
        };
        if end <= self.cursor {
            return false;
        }
        self.kill_buffer = self.text[self.cursor..end].to_string();
        let start = self.cursor;
        self.text.replace_range(start..end, "");
        self.preferred_column = None;
        self.reshape_chips(start, end);
        self.shift_chips(start, -((end - start) as isize));
        self.record(EditKind::Block);
        true
    }

    /// Kill from the start of the line to the cursor, for `Ctrl+U`.
    pub fn kill_to_line_start(&mut self) -> bool {
        if let Some(selected) = self.selected_expanded_text()
            && self.delete_selection()
        {
            self.kill_buffer = selected;
            self.record(EditKind::Block);
            return true;
        }
        let (line_start, _) = self.cursor_line();
        if line_start == self.cursor {
            return false;
        }
        self.kill_buffer = self.text[line_start..self.cursor].to_string();
        let end = self.cursor;
        self.text.replace_range(line_start..end, "");
        self.cursor = line_start;
        self.preferred_column = None;
        self.reshape_chips(line_start, end);
        self.shift_chips(line_start, -((end - line_start) as isize));
        self.record(EditKind::Block);
        true
    }

    /// Put the last killed text back at the cursor, for `Ctrl+Y`.
    pub fn yank(&mut self) -> bool {
        if self.kill_buffer.is_empty() {
            return false;
        }
        self.delete_selection();
        let text = self.kill_buffer.clone();
        let at = self.cursor;
        self.text.insert_str(at, &text);
        self.cursor += text.len();
        self.preferred_column = None;
        self.shift_chips(at, text.len() as isize);
        self.record(EditKind::Block);
        true
    }

    /// Move to the start of the previous word, for `Alt+B`.
    pub fn move_word_left(&mut self) {
        self.selection = None;
        self.cursor = self.word_start(self.cursor);
        self.preferred_column = None;
        self.break_batch();
    }

    /// Move past the end of the next word, for `Alt+F`.
    pub fn move_word_right(&mut self) {
        self.selection = None;
        self.cursor = self.word_end(self.cursor);
        self.preferred_column = None;
        self.break_batch();
    }

    /// The byte range of the line the cursor is on, without its newline.
    fn cursor_line(&self) -> (usize, usize) {
        let (line, _) = self.cursor_line_column();
        self.line_range(line)
    }

    /// The start of the word before `index`, skipping whitespace.
    fn word_start(&self, index: usize) -> usize {
        let mut index = self.skip_whitespace_back(index);
        if index == 0 {
            return 0;
        }
        let previous = self.prev_boundary_from(index);
        let class = word_class(&self.text[previous..index]);
        while index > 0 {
            let previous = self.prev_boundary_from(index);
            if word_class(&self.text[previous..index]) != class {
                break;
            }
            index = previous;
        }
        index
    }

    /// The end of the word at or after `index`, skipping leading whitespace.
    fn word_end(&self, index: usize) -> usize {
        let mut index = self.skip_whitespace_forward(index);
        if index >= self.text.len() {
            return self.text.len();
        }
        let next = self.next_boundary_from(index);
        let class = word_class(&self.text[index..next]);
        while index < self.text.len() {
            let next = self.next_boundary_from(index);
            if word_class(&self.text[index..next]) != class {
                break;
            }
            index = next;
        }
        index
    }

    /// The start of the whitespace-delimited word before the cursor.
    fn whitespace_word_start(&self) -> usize {
        let mut start = self.cursor;
        start = self.skip_whitespace_back(start);
        while start > 0 {
            let previous = self.prev_boundary_from(start);
            if self.text[previous..start].chars().all(char::is_whitespace) {
                break;
            }
            start = previous;
        }
        start
    }

    fn skip_whitespace_back(&self, mut index: usize) -> usize {
        while index > 0 {
            let previous = self.prev_boundary_from(index);
            if !self.text[previous..index].chars().all(char::is_whitespace) {
                break;
            }
            index = previous;
        }
        index
    }

    fn skip_whitespace_forward(&self, mut index: usize) -> usize {
        while index < self.text.len() {
            let next = self.next_boundary_from(index);
            if !self.text[index..next].chars().all(char::is_whitespace) {
                break;
            }
            index = next;
        }
        index
    }

    fn next_boundary_from(&self, index: usize) -> usize {
        self.text[index..]
            .grapheme_indices(true)
            .next()
            .map(|(_, grapheme)| index + grapheme.len())
            .unwrap_or(self.text.len())
    }

    fn previous_grapheme_boundary(&self) -> usize {
        self.prev_boundary_from(self.cursor)
    }

    fn prev_boundary_from(&self, index: usize) -> usize {
        self.text[..index]
            .grapheme_indices(true)
            .next_back()
            .map(|(offset, _)| offset)
            .unwrap_or(0)
    }

    fn next_grapheme_boundary(&self) -> usize {
        self.text[self.cursor..]
            .grapheme_indices(true)
            .next()
            .map(|(_, grapheme)| self.cursor + grapheme.len())
            .unwrap_or(self.text.len())
    }

    pub fn move_left(&mut self) {
        self.break_batch();
        self.selection = None;
        // A chip is one stop, not one per label character.
        self.cursor = match self.chips.iter().find(|chip| chip.end == self.cursor) {
            Some(chip) => chip.start,
            None => self.previous_grapheme_boundary(),
        };
        self.preferred_column = None;
    }

    pub fn move_right(&mut self) {
        self.break_batch();
        self.selection = None;
        self.cursor = match self.chips.iter().find(|chip| chip.start == self.cursor) {
            Some(chip) => chip.end,
            None => self.next_grapheme_boundary(),
        };
        self.preferred_column = None;
    }

    /// Where the cursor sits in the wrapped display grid: `(row, column)`.
    ///
    /// This is the inverse of [`ComposerBuffer::move_cursor_to_cell`], and the
    /// renderer needs it to place the terminal's own cursor on the draft. A
    /// cursor exactly on a wrap boundary belongs to the start of the next row,
    /// which is where the next character will appear.
    ///
    /// The rows come from [`crate::text::wrap_source_text`], whose lines are
    /// verbatim slices of the draft, so the cursor's byte offset can be used on
    /// the row's text directly. [`floor_boundary`] is the belt to that braces:
    /// a mismatch has to degrade the caret's column, never abort the process.
    pub fn cursor_cell(&self, width: usize) -> (u16, u16) {
        let width = width.max(1);
        let mut row = 0usize;
        for line in 0..self.line_count() {
            let (start, end) = self.line_range(line);
            let wrapped = crate::text::wrap_source_text(&self.text[start..end], width);
            let segments = wrapped.len().max(1);
            if self.cursor >= start && self.cursor <= end {
                for (index, segment) in wrapped.iter().enumerate() {
                    let segment_start = start + segment.source_start;
                    let segment_end = segment_start + segment.text.len();
                    if self.cursor < segment_end || index + 1 == wrapped.len() {
                        let offset = floor_boundary(
                            &segment.text,
                            self.cursor
                                .saturating_sub(segment_start)
                                .min(segment.text.len()),
                        );
                        let column = display_width(&segment.text[..offset]);
                        return ((row + index) as u16, column as u16);
                    }
                }
                return ((row + segments - 1) as u16, 0);
            }
            row += segments;
        }
        (row as u16, 0)
    }

    /// Put the cursor on a display cell, for a click in the composer.
    ///
    /// `row` counts wrapped display rows, which is what the renderer laid out;
    /// the mapping back to a logical line is done by walking the same wrapping
    /// the renderer used.
    pub fn move_cursor_to_cell(&mut self, row: u16, column: u16) {
        self.selection = None;
        let width = self.last_display_width.max(1);
        let mut remaining = usize::from(row);
        for line in 0..self.line_count() {
            let (start, end) = self.line_range(line);
            let wrapped = crate::text::wrap_source_text(&self.text[start..end], width);
            let segments = wrapped.len().max(1);
            if remaining < segments {
                let offset = wrapped
                    .get(remaining)
                    .map(|segment| segment.source_start)
                    .unwrap_or(0);
                let segment_text = wrapped
                    .get(remaining)
                    .map(|segment| segment.text.clone())
                    .unwrap_or_default();
                let column = byte_offset_for_column(&segment_text, usize::from(column));
                self.cursor = start + offset + column;
                self.preferred_column = None;
                self.break_batch();
                return;
            }
            remaining -= segments;
        }
        self.cursor = self.text.len();
        self.preferred_column = None;
        self.break_batch();
    }

    /// Line index and column (in display columns) of the cursor.
    pub fn cursor_line_column(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.matches('\n').count();
        let line_start = before.rfind('\n').map(|index| index + 1).unwrap_or(0);
        (line, display_width(&before[line_start..]))
    }

    pub fn line_count(&self) -> usize {
        self.text.matches('\n').count() + 1
    }

    /// How many rows the draft fills when wrapped to `width`.
    ///
    /// [`ComposerBuffer::line_count`] counts newlines, so one long line is one
    /// line to it and several rows on screen. The composer's band is sized from
    /// this instead: a draft that wraps needs the rows it wraps to, or the
    /// renderer paints the first one and clips the rest.
    pub fn display_row_count(&self, width: usize) -> usize {
        let width = width.max(1);
        self.text
            .split('\n')
            .map(|line| crate::text::wrap_source_text(line, width).len())
            .sum::<usize>()
            .max(1)
    }

    /// The byte range of a line, without its newline.
    pub fn line_range(&self, line: usize) -> (usize, usize) {
        let mut start = 0usize;
        for _ in 0..line {
            match self.text[start..].find('\n') {
                Some(offset) => start += offset + 1,
                None => return (self.text.len(), self.text.len()),
            }
        }
        let end = self.text[start..]
            .find('\n')
            .map(|offset| start + offset)
            .unwrap_or(self.text.len());
        (start, end)
    }

    pub fn move_up(&mut self) {
        self.break_batch();
        self.selection = None;
        let (line, column) = self.cursor_line_column();
        if line == 0 {
            return;
        }
        let target_column = self.preferred_column.unwrap_or(column);
        let (start, end) = self.line_range(line - 1);
        self.cursor = start + byte_offset_for_column(&self.text[start..end], target_column);
        self.preferred_column = Some(target_column);
    }

    pub fn move_down(&mut self) {
        self.break_batch();
        self.selection = None;
        let (line, column) = self.cursor_line_column();
        if line + 1 >= self.line_count() {
            return;
        }
        let target_column = self.preferred_column.unwrap_or(column);
        let (start, end) = self.line_range(line + 1);
        self.cursor = start + byte_offset_for_column(&self.text[start..end], target_column);
        self.preferred_column = Some(target_column);
    }

    pub fn move_line_start(&mut self) {
        self.break_batch();
        self.selection = None;
        let (line, _) = self.cursor_line_column();
        self.cursor = self.line_range(line).0;
        self.preferred_column = None;
    }

    pub fn move_line_end(&mut self) {
        self.break_batch();
        self.selection = None;
        let (line, _) = self.cursor_line_column();
        self.cursor = self.line_range(line).1;
        self.preferred_column = None;
    }

    pub fn move_to_start(&mut self) {
        self.break_batch();
        self.selection = None;
        self.cursor = 0;
        self.preferred_column = None;
    }

    pub fn move_to_end(&mut self) {
        self.break_batch();
        self.selection = None;
        self.cursor = self.text.len();
        self.preferred_column = None;
    }

    /// The word immediately before the cursor, plus its byte range.
    pub fn word_before_cursor(&self) -> (usize, &str) {
        let mut start = self.cursor;
        while start > 0 {
            let previous = self.prev_boundary_from(start);
            let grapheme = &self.text[previous..start];
            if grapheme.chars().all(|character| character.is_whitespace()) {
                break;
            }
            start = previous;
        }
        (start, &self.text[start..self.cursor])
    }

    /// Detect an unterminated trigger word at the cursor.
    ///
    /// A trigger opens a *word*: `and/or` is not a command, and neither is the
    /// `@` inside an address. Where the word is does not matter — `/` after a
    /// word on the same line opens the drawer exactly as it does at the start of
    /// a line, which is the rule the desktop composer applies, so the two
    /// clients answer the same keystrokes the same way.
    ///
    /// What follows the trigger is its query, and a query that repeats the
    /// trigger is not a trigger at all: a second `/` closes the one before it
    /// rather than opening another, which is what keeps `/usr/bin` and `//`
    /// from asking the authority about a command nobody named.
    pub fn active_trigger(&self) -> Option<(CompletionTrigger, usize, String)> {
        let (start, word) = self.word_before_cursor();
        let mut characters = word.chars();
        let first = characters.next()?;
        let trigger = CompletionTrigger::from_character(first)?;
        if start > 0 {
            let previous = self.prev_boundary_from(start);
            let before = &self.text[previous..start];
            if !before.chars().all(char::is_whitespace) {
                return None;
            }
        }
        let query = characters.as_str();
        if query.contains(trigger.character()) {
            return None;
        }
        Some((trigger, start, query.to_string()))
    }

    /// Replace the active trigger word with `insertion`.
    pub fn replace_trigger_word(&mut self, start: usize, insertion: &str) {
        self.text.replace_range(start..self.cursor, insertion);
        self.cursor = start + insertion.len();
        self.preferred_column = None;
    }

    /// Attach a path to the draft, quoting it when it contains spaces.
    pub fn attach_path(&mut self, path: &str) {
        if !self.text.is_empty() && !self.text.ends_with([' ', '\n']) {
            self.insert_char(' ');
        }
        if path.contains(char::is_whitespace) {
            self.insert_str(&format!("\"{path}\""));
        } else {
            self.insert_str(path);
        }
        self.insert_char(' ');
    }

    /// Paths the draft references, used to show attachment chips.
    ///
    /// Splitting is quote-aware so `"/tmp/a b.txt"` stays one attachment.
    pub fn attachment_candidates(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut current = String::new();
        let mut quoted = false;
        let flush = |current: &mut String, out: &mut Vec<String>| {
            if current.is_empty() {
                return;
            }
            let token = std::mem::take(current);
            if token.starts_with('/')
                || token.starts_with("./")
                || token.starts_with("../")
                || token.starts_with("~/")
            {
                out.push(token);
            }
        };
        for character in self.text.chars() {
            match character {
                '"' => {
                    quoted = !quoted;
                    if !quoted {
                        flush(&mut current, &mut out);
                    }
                }
                character if character.is_whitespace() && !quoted => {
                    flush(&mut current, &mut out);
                }
                character => current.push(character),
            }
        }
        flush(&mut current, &mut out);
        out
    }

    /// The visual rows of the buffer, wrapped to `width`, with their origin.
    ///
    /// The rows are wrapped with [`crate::text::wrap_source_text`] rather than
    /// [`crate::text::wrap_text`]: everything painted onto a row — the caret,
    /// the selection, a chip's label — is addressed by a byte offset in the
    /// draft, so a row's text has to be the slice of the draft it came from.
    pub fn display_rows(&mut self, width: usize) -> Vec<DisplayRow> {
        self.last_display_width = width.max(1);
        let (cursor_line, _) = self.cursor_line_column();
        let mut out = Vec::new();
        for line in 0..self.line_count() {
            let (start, end) = self.line_range(line);
            let text = &self.text[start..end];
            let wrapped = crate::text::wrap_source_text(text, width.max(1));
            for segment in wrapped {
                out.push(DisplayRow {
                    text: segment.text,
                    cursor_line: line == cursor_line,
                    source_start: start + segment.source_start,
                });
            }
        }
        if out.is_empty() {
            out.push(DisplayRow {
                text: String::new(),
                cursor_line: true,
                source_start: 0,
            });
        }
        out
    }

    /// The visual lines of the buffer, wrapped to `width`.
    pub fn display_lines(&mut self, width: usize) -> Vec<(String, bool)> {
        self.display_rows(width)
            .into_iter()
            .map(|row| (row.text, row.cursor_line))
            .collect()
    }
}

/// A draft on its way out: the text, and every image with its place in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub text: String,
    /// Each attached image and the UTF-16 offset in `text` where it sat.
    pub images: Vec<(ImageAttachment, u32)>,
}

/// The wire form of an attached image.
///
/// Two things have to be right for the other clients to *show* the picture
/// rather than mention it. The URI has to name something they can read: a
/// `file://` URI — the same form the desktop writes — absolute so a client with
/// a different working directory still finds it. And the offset has to say where
/// in the message the picture was, or every reader that honours it puts it at
/// the end of the paragraph instead of where it was written.
///
/// An image read from the clipboard has nowhere to live but the message, so it
/// travels as a data URL — unless this client *is* the authority, in which case
/// the bytes are written beside it and the file is named instead: the runtime
/// that has to read them is this host, and a path is the only form the desktop
/// can draw.
pub fn message_attachment(
    image: &ImageAttachment,
    inline_text_offset: u32,
    materialise_bytes: bool,
) -> vibex_core::MessageAttachment {
    let uri = match &image.source {
        ImageSource::Path(path) => file_uri(path),
        ImageSource::Bytes(bytes) if materialise_bytes => {
            materialise_attachment(&image.mime_type, bytes)
                .map(|path| file_uri(&path))
                .unwrap_or_else(|| data_uri(&image.mime_type, bytes))
        }
        ImageSource::Bytes(bytes) => data_uri(&image.mime_type, bytes),
    };
    vibex_core::MessageAttachment {
        label: image.label.clone(),
        mime_type: Some(image.mime_type.clone()),
        uri: Some(uri),
        inline_text_offset: Some(inline_text_offset),
    }
}

fn data_uri(mime_type: &str, bytes: &[u8]) -> String {
    format!(
        "data:{};base64,{}",
        mime_type,
        crate::terminal::encode_base64(bytes)
    )
}

/// A path as a `file://` URI, absolute so any client can resolve it.
fn file_uri(path: &str) -> String {
    let path = std::path::Path::new(path);
    let absolute = path
        .is_absolute()
        .then(|| path.to_path_buf())
        .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(path)));
    match absolute {
        Some(path) => format!("file://{}", path.display()),
        None => format!("file://{path}", path = path.display()),
    }
}

/// Write clipboard bytes to a file the runtime and the desktop can both read.
///
/// Named by content, so attaching the same screenshot twice writes it once, and
/// beside the OS's own temporary files, which is where a message's bytes belong
/// when the message is the only owner. `None` when the platform will not have
/// it: the data URL still reaches the Agent, it is only the picture that the
/// other clients cannot draw.
///
/// Shared with [`crate::queue_prefs`], which has to file a held message's
/// pictures somewhere before it can write the queue down: a state file carrying
/// raw pixels would grow by megabytes for one screenshot.
pub(crate) fn materialise_attachment(mime_type: &str, bytes: &[u8]) -> Option<String> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    mime_type.hash(&mut hasher);
    bytes.hash(&mut hasher);
    let extension = crate::composer::image_mime_for_path(mime_type)
        .or_else(|| mime_type.strip_prefix("image/").map(|_| mime_type))
        .map(|mime| mime.rsplit('/').next().unwrap_or("png").to_string())
        .unwrap_or_else(|| "png".to_string());
    let directory = std::env::temp_dir().join("vibex-attachments");
    std::fs::create_dir_all(&directory).ok()?;
    let path = directory.join(format!("{:016x}.{}", hasher.finish(), extension));
    if !path.exists() {
        std::fs::write(&path, bytes).ok()?;
    }
    Some(path.display().to_string())
}

/// The image type for a path, when its extension names one.
pub fn image_mime_for_path(path: &str) -> Option<&'static str> {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// The byte index in `text` of a UTF-16 offset, clamped to its length.
///
/// `inline_text_offset` is counted in UTF-16 units because that is what the
/// other clients — and the web platform they were written against — count in;
/// the buffer counts bytes, and a message full of CJK is where the two disagree.
pub fn utf16_offset_to_byte(text: &str, offset: usize) -> usize {
    let mut utf16 = 0usize;
    for (byte, character) in text.char_indices() {
        if utf16 >= offset {
            return byte;
        }
        utf16 += character.len_utf16();
    }
    text.len()
}

fn byte_offset_for_column(line: &str, column: usize) -> usize {
    if column == 0 {
        return 0;
    }
    let (prefix, _) = take_width(line, column);
    prefix.len()
}

/// The largest character boundary at or below `offset`.
///
/// A caret is placed by slicing text at an offset derived from the draft, and
/// slicing at a non-boundary panics. The wrapping already guarantees the
/// offsets line up, so this only ever has to hold when something else is wrong;
/// moving the caret back one character is a bad frame, aborting is a lost
/// session.
fn floor_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// The messages this client sent, for the `? ` history drawer.
///
/// The arrows no longer walk this: they walk the *session's* timeline, so a
/// message another client sent is recallable too. The drawer is a search over
/// what this reader typed, which is a different question — it is a vocabulary
/// of one's own — so it keeps its own bounded, deduplicated list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposerLog {
    entries: Vec<String>,
    limit: usize,
}

impl ComposerLog {
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            entries: Vec::new(),
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn push(&mut self, entry: impl Into<String>) {
        let entry = entry.into();
        if entry.trim().is_empty() {
            return;
        }
        self.entries.retain(|existing| existing != &entry);
        self.entries.push(entry);
        while self.entries.len() > self.limit {
            self.entries.remove(0);
        }
    }
}

/// What one press of `Up`/`Down` did to the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryStep {
    /// Put this text in the composer.
    Text(String),
    /// The press was the walk's, but it put nothing new in the composer: the
    /// oldest message is sticky, and the caret must not wander inside it.
    Sticky,
    /// The press was not the walk's, so `Up`/`Down` keep their normal meaning.
    Ignored,
}

/// `Up`/`Down` recall of the selected session's earlier user messages.
///
/// The list is the *session's*: the messages are the user rows of the session's
/// own timeline, handed to the walk at the moment a key is pressed rather than
/// accumulated here — the same pool the desktop's composer draws from. That is
/// what makes a message another client sent recallable too, and what keeps a
/// session that was cleared or reloaded from leaving a stale message behind the
/// cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposerHistory {
    /// `None` means "editing a fresh draft"; `Some(index)` walks backwards
    /// through the list, newest last.
    index: Option<usize>,
    /// What the draft held when the walk started, restored when the walk steps
    /// past the newest message or the reader leaves it.
    draft: String,
    /// The text the walk last put in the composer.
    ///
    /// It is what tells the walk's own step apart from the reader's edit: once
    /// the box holds anything else, the recalled message has become live text
    /// and the arrows go back to moving the caret.
    installed: String,
    /// The session the walk is pinned to, so switching sessions cannot resume a
    /// stale cursor on a message the reader can no longer see.
    session_id: Option<VibexSessionId>,
}

impl ComposerHistory {
    /// Whether a walk is running on `session_id`.
    ///
    /// A begun walk counts even before it has stepped anywhere: what pins it to
    /// a composer is the session it belongs to, not how far it has walked.
    pub fn is_browsing(&self, session_id: Option<&VibexSessionId>) -> bool {
        session_id.is_some() && self.session_id.as_ref() == session_id
    }

    /// Begin a walk on `session_id`, remembering the draft it displaces.
    ///
    /// This is the caller's decision rather than the walk's: the composer owns
    /// the rule that `Up` only starts one from an empty box, and a caller that
    /// starts one from a live draft already knows what it is doing.
    pub fn begin(&mut self, session_id: &VibexSessionId, current: &str) {
        self.index = None;
        self.installed = current.to_string();
        self.draft = current.to_string();
        self.session_id = Some(session_id.clone());
    }

    /// Step to an older entry. `entries` is the session's user messages, oldest
    /// first; `current` is the text the composer holds now.
    pub fn previous(
        &mut self,
        session_id: &VibexSessionId,
        entries: &[String],
        current: &str,
    ) -> HistoryStep {
        self.end_if_edited(session_id, current);
        let active = self.is_browsing(Some(session_id));
        if !active {
            return HistoryStep::Ignored;
        }
        // A cursor whose message left the timeline cannot name a row: the
        // session was cleared or reloaded, so the walk ends rather than
        // pointing at nothing.
        match self.index {
            Some(index) if index >= entries.len() => {
                self.reset();
                HistoryStep::Ignored
            }
            // The oldest message is sticky rather than wrapping around to the
            // newest.
            Some(0) if !entries.is_empty() => HistoryStep::Sticky,
            Some(index) => self.install(index - 1, entries),
            None => match entries.is_empty() {
                true => {
                    self.reset();
                    HistoryStep::Sticky
                }
                false => self.install(entries.len() - 1, entries),
            },
        }
    }

    /// Step to a newer entry, restoring the draft past the newest message.
    ///
    /// `Down` never starts a walk on its own, so an untouched composer keeps
    /// the caret's own downward movement.
    #[allow(clippy::should_implement_trait)]
    pub fn next(
        &mut self,
        session_id: &VibexSessionId,
        entries: &[String],
        current: &str,
    ) -> HistoryStep {
        self.end_if_edited(session_id, current);
        let Some(index) = self.index.filter(|_| self.is_browsing(Some(session_id))) else {
            return HistoryStep::Ignored;
        };
        if index + 1 >= entries.len() {
            // Past the newest message: the walk ends and the draft returns.
            let draft = std::mem::take(&mut self.draft);
            self.reset();
            return HistoryStep::Text(draft);
        }
        self.install(index + 1, entries)
    }

    /// End the walk, handing back the draft it was holding.
    ///
    /// `None` means no walk was running, so the caller leaves the composer
    /// alone rather than clearing text the reader never left.
    pub fn cancel(&mut self) -> Option<String> {
        self.session_id.as_ref()?;
        let draft = std::mem::take(&mut self.draft);
        self.reset();
        Some(draft)
    }

    /// Forget the walk without handing anything back.
    pub fn reset(&mut self) {
        self.index = None;
        self.draft.clear();
        self.installed.clear();
        self.session_id = None;
    }

    /// End a walk the reader has typed into, so an edit is never replaced by a
    /// stale draft on the next press.
    fn end_if_edited(&mut self, session_id: &VibexSessionId, current: &str) {
        if self.is_browsing(Some(session_id)) && self.installed != current {
            self.reset();
        }
    }

    fn install(&mut self, index: usize, entries: &[String]) -> HistoryStep {
        let text = entries[index].clone();
        self.index = Some(index);
        self.installed = text.clone();
        HistoryStep::Text(text)
    }
}

/// Turn any line break a terminal might send into `\n`.
///
/// Bracketed paste delivers what the clipboard holds, and that can include
/// bare carriage returns (old Mac line endings), `\r\n`, or the Unicode line
/// and paragraph separators. Normalising once, here, means everything
/// downstream can assume `\n`.
pub fn normalize_line_breaks(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                out.push('\n');
            }
            '\u{2028}' | '\u{2029}' => out.push('\n'),
            other => out.push(other),
        }
    }
    out
}

/// The label a collapsed paste is shown as.
fn paste_label(value: &str, lines: usize) -> String {
    if lines == 1 && value.len() > PASTE_CHIP_BYTES {
        return format!("[Pasted: {}]", compact_bytes(value.len()));
    }
    if lines == 1 {
        "[Pasted: 1 line]".to_string()
    } else {
        format!("[Pasted: {lines} lines]")
    }
}

/// Decimal byte count, the way a file manager reports one.
pub fn compact_bytes(bytes: usize) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{bytes} bytes")
    }
}

/// Which class a grapheme belongs to, for word motions.
///
/// `Small` word style: a run of alphanumerics and underscores, a run of
/// punctuation, or a run of whitespace. `src/net.rs` is therefore five stops —
/// `src`, `/`, `net`, `.`, `rs` — which is what makes `Alt+F` usable for
/// editing a path without reaching for the arrow keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordClass {
    Word,
    Punctuation,
    Space,
}

fn word_class(grapheme: &str) -> WordClass {
    let mut characters = grapheme.chars();
    match characters.next() {
        None => WordClass::Space,
        Some(character) if character.is_whitespace() => WordClass::Space,
        Some(character) if character.is_alphanumeric() || character == '_' => WordClass::Word,
        Some(_) => WordClass::Punctuation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(text: &str) -> ComposerBuffer {
        ComposerBuffer::from_text(text)
    }

    #[test]
    fn insertion_and_backspace_are_grapheme_correct() {
        let mut buffer = ComposerBuffer::default();
        for character in "e\u{301}clair".chars() {
            buffer.insert_char(character);
        }
        assert_eq!(buffer.text(), "e\u{301}clair");
        // Deleting once removes the combining accent together with its base.
        buffer.backspace();
        assert_eq!(buffer.text(), "e\u{301}clai");
        // Deleting at the start of the line removes the base and its accent.
        buffer.move_to_start();
        buffer.delete();
        assert_eq!(buffer.text(), "clai");
    }

    #[test]
    fn cjk_characters_are_inserted_and_removed_whole() {
        let mut buffer = ComposerBuffer::default();
        buffer.insert_str("中文输入");
        assert_eq!(buffer.text(), "中文输入");
        buffer.backspace();
        assert_eq!(buffer.text(), "中文输");
        let (_, column) = buffer.cursor_line_column();
        assert_eq!(column, 6);
    }

    #[test]
    fn vertical_movement_keeps_the_preferred_column() {
        let mut buffer = buffer("a long first line\nshort\nanother long line");
        buffer.move_to_start();
        // Move to the end of line 1, then down past the short line.
        for _ in 0..10 {
            buffer.move_right();
        }
        let (line, column) = buffer.cursor_line_column();
        assert_eq!(line, 0);
        buffer.move_down();
        assert_eq!(buffer.cursor_line_column().0, 1);
        buffer.move_down();
        let (line, column_after) = buffer.cursor_line_column();
        assert_eq!(line, 2);
        assert_eq!(column_after, column.min(display_width("another long line")));
    }

    #[test]
    fn word_deletion_matches_shell_behaviour() {
        let mut buffer = buffer("git commit -m message");
        assert!(buffer.delete_word_before());
        assert_eq!(buffer.text(), "git commit -m ");
        assert!(buffer.delete_word_before());
        assert_eq!(buffer.text(), "git commit ");
        assert!(buffer.delete_word_before());
        assert_eq!(buffer.text(), "git ");
        buffer.move_to_start();
        assert!(!buffer.delete_word_before());
    }

    #[test]
    fn triggers_are_detected_only_at_a_word_start() {
        let draft = buffer("/he");
        assert_eq!(
            draft.active_trigger(),
            Some((CompletionTrigger::Slash, 0, "he".to_string()))
        );

        let draft = buffer("look @src/ma");
        assert_eq!(
            draft.active_trigger(),
            Some((CompletionTrigger::At, 5, "src/ma".to_string()))
        );

        // A slash inside a word is just a slash.
        let draft = buffer("and/or");
        assert_eq!(draft.active_trigger(), None);
    }

    #[test]
    fn a_trigger_opens_after_anything_a_word_can_follow() {
        // Regression: `/` was a command only at the start of a *line*, so a
        // reader who had already typed on that line got no drawer at all until
        // they pressed Enter and started a new one. The rule is the start of a
        // *word*, which is also what the desktop composer applies.
        for text in ["testest /", "run /compact", "look at @src", "use $skill"] {
            let draft = buffer(text);
            assert!(
                draft.active_trigger().is_some(),
                "{text:?} opened no drawer"
            );
        }

        // A path is not a command: the query would carry the trigger's own
        // character, and a second `/` closes the first rather than opening a
        // popup for a directory listing nobody asked for.
        let draft = buffer("cat /etc/hosts");
        assert_eq!(draft.active_trigger(), None);
        let draft = buffer("open //tmp");
        assert_eq!(draft.active_trigger(), None);

        // Neither is a second `@` inside a reference.
        let draft = buffer("@src/@lib");
        assert_eq!(draft.active_trigger(), None);
    }

    #[test]
    fn the_drawer_only_shows_rows_the_query_still_matches() {
        let menu = CompletionMenu {
            trigger: CompletionTrigger::At,
            start: 0,
            query: "src".to_string(),
            lists: CompletionLists {
                files: vec![
                    Completion {
                        insert: "@crates/vibex-tui/src/run.rs ".into(),
                        label: "@run.rs".into(),
                        detail: "src/run.rs".into(),
                        group: "Reference".into(),
                    },
                    Completion {
                        insert: "@README.md ".into(),
                        label: "@README.md".into(),
                        detail: String::new(),
                        group: "Reference".into(),
                    },
                ],
                ..CompletionLists::default()
            },
            tab: CompletionTab::Files,
            selected: 0,
            loading: false,
        };
        // The label alone would not match: the directory the reader typed is in
        // the insertion, which is what the authority matched the query against.
        assert_eq!(menu.visible(), vec![0]);

        // The selection can only stand on a row the drawer draws; a query that
        // filters the selected row out moves it to the first row that survives.
        let mut menu = menu;
        menu.reseat("read".to_string());
        assert_eq!(menu.visible(), vec![1]);
        assert_eq!(menu.selected, 1);
        assert_eq!(
            menu.selected_item().map(|item| item.label.as_str()),
            Some("@README.md")
        );

        // A query nothing matches leaves the drawer with nothing to offer, so
        // `Enter` sends the draft rather than inserting an invisible row.
        menu.reseat("nothing-matches-this".to_string());
        assert!(menu.visible().is_empty());
        assert!(menu.selected_item().is_none());
    }

    #[test]
    fn accepting_a_completion_replaces_the_trigger_word() {
        let mut draft = buffer("/he");
        let (_, start, _) = draft.active_trigger().unwrap();
        draft.replace_trigger_word(start, "/help ");
        assert_eq!(draft.text(), "/help ");
    }

    #[test]
    fn panels_follow_the_trigger_and_clamp_at_their_edges() {
        // The same panels the desktop popup opens, in the same order: `/`
        // carries the commands and the reusable Prompts, `@` the workspace
        // files, the collaborators and the conversations, and `$` keeps its one
        // Skills panel.
        assert_eq!(
            CompletionTab::tabs(CompletionTrigger::Slash),
            &[CompletionTab::Commands, CompletionTab::Phrases]
        );
        assert_eq!(
            CompletionTab::tabs(CompletionTrigger::At),
            &[
                CompletionTab::Files,
                CompletionTab::Delegation,
                CompletionTab::Sessions
            ]
        );
        assert_eq!(
            CompletionTab::tabs(CompletionTrigger::Dollar),
            &[CompletionTab::Commands]
        );

        let slash = CompletionTab::tabs(CompletionTrigger::Slash);
        assert_eq!(
            CompletionTab::Commands.moved(slash, true),
            CompletionTab::Phrases
        );
        // At the edge the panel does not move: the key is left for the caret,
        // so `Left` on the first panel still moves inside the query.
        assert_eq!(
            CompletionTab::Commands.moved(slash, false),
            CompletionTab::Commands
        );
        assert_eq!(
            CompletionTab::Phrases.moved(slash, true),
            CompletionTab::Phrases
        );
    }

    #[test]
    fn a_menu_keeps_one_list_per_panel_and_switches_between_them() {
        let row = |label: &str| Completion {
            insert: format!("{label} "),
            label: label.to_string(),
            detail: String::new(),
            group: "Command".to_string(),
        };
        let mut menu = CompletionMenu {
            trigger: CompletionTrigger::Slash,
            start: 0,
            query: String::new(),
            lists: CompletionLists {
                commands: vec![row("/compact"), row("/clear")],
                phrases: vec![row("/review")],
                ..CompletionLists::default()
            },
            tab: CompletionTab::Commands,
            selected: 1,
            loading: false,
        };

        assert!(menu.tabs_available());
        assert_eq!(menu.active_tab(), CompletionTab::Commands);
        assert_eq!(menu.items().len(), 2);

        // The switch reads the other list, which travelled in the same answer,
        // and the selection starts over rather than keeping the first panel's
        // index.
        assert!(menu.step_tab(true));
        assert_eq!(menu.active_tab(), CompletionTab::Phrases);
        assert_eq!(
            menu.selected_item().map(|item| item.label.as_str()),
            Some("/review")
        );

        // The last panel clamps, so the key is reported as unhandled.
        assert!(!menu.step_tab(true));
        assert!(menu.step_tab(false));
        assert_eq!(menu.active_tab(), CompletionTab::Commands);

        // A trigger with one panel has nothing to switch, whatever the menu
        // remembers from an earlier one.
        menu.trigger = CompletionTrigger::Dollar;
        menu.tab = CompletionTab::Phrases;
        assert!(!menu.tabs_available());
        assert_eq!(menu.active_tab(), CompletionTab::Commands);
        assert!(!menu.step_tab(true));
    }

    #[test]
    fn attachments_are_quoted_when_they_contain_spaces() {
        let mut buffer = ComposerBuffer::default();
        buffer.attach_path("/tmp/a b.txt");
        buffer.attach_path("/tmp/c.txt");
        assert_eq!(buffer.text(), "\"/tmp/a b.txt\" /tmp/c.txt ");
        assert_eq!(
            buffer.attachment_candidates(),
            vec!["/tmp/a b.txt".to_string(), "/tmp/c.txt".to_string()]
        );
    }

    #[test]
    fn history_walks_back_and_restores_the_draft() {
        let session = vibex_core::VibexSessionId::new();
        let entries = vec!["first".to_string(), "second".to_string()];
        let mut history = ComposerHistory::default();
        history.begin(&session, "my draft");
        assert_eq!(
            history.previous(&session, &entries, "my draft"),
            HistoryStep::Text("second".to_string())
        );
        assert_eq!(
            history.previous(&session, &entries, "second"),
            HistoryStep::Text("first".to_string())
        );
        // The oldest message is sticky: the press is the walk's, but it puts
        // nothing new in the composer.
        assert_eq!(
            history.previous(&session, &entries, "first"),
            HistoryStep::Sticky
        );
        assert_eq!(
            history.next(&session, &entries, "first"),
            HistoryStep::Text("second".to_string())
        );
        // Past the newest message the draft comes back, and the walk ends.
        assert_eq!(
            history.next(&session, &entries, "second"),
            HistoryStep::Text("my draft".to_string())
        );
        assert!(!history.is_browsing(Some(&session)));
    }

    #[test]
    fn an_unstarted_walk_ignores_both_arrows() {
        let session = vibex_core::VibexSessionId::new();
        let entries = vec!["sent".to_string()];
        let mut history = ComposerHistory::default();
        // Neither arrow starts a walk: the caller begins one, and only from a
        // composer it means to walk (the composer's own rule is `Up` from an
        // empty box). An idle walk therefore has nothing to say about either.
        assert_eq!(
            history.previous(&session, &entries, ""),
            HistoryStep::Ignored
        );
        assert_eq!(history.next(&session, &entries, ""), HistoryStep::Ignored);
        // A walk that is running keeps the keys even while text is in the box:
        // that text is a recalled message the reader may step away from.
        history.begin(&session, "");
        history.previous(&session, &entries, "");
        assert_eq!(
            history.next(&session, &entries, "sent"),
            HistoryStep::Text(String::new())
        );
    }

    #[test]
    fn typing_into_a_recalled_message_ends_the_walk() {
        let session = vibex_core::VibexSessionId::new();
        let entries = vec!["first".to_string(), "second".to_string()];
        let mut history = ComposerHistory::default();
        history.begin(&session, "my draft");
        history.previous(&session, &entries, "my draft");
        // The reader edits what came back: it is live text now, and the arrows
        // must not replace the edit with a stale draft.
        assert_eq!(
            history.next(&session, &entries, "second, edited"),
            HistoryStep::Ignored
        );
        assert!(!history.is_browsing(Some(&session)));
    }

    #[test]
    fn history_is_pinned_to_one_session() {
        let first = vibex_core::VibexSessionId::new();
        let second = vibex_core::VibexSessionId::new();
        let entries = vec!["from the first session".to_string()];
        let mut history = ComposerHistory::default();
        history.begin(&first, "");
        assert!(history.is_browsing(Some(&first)));
        assert_eq!(
            history.previous(&first, &entries, ""),
            HistoryStep::Text("from the first session".to_string())
        );
        // Another session's composer is a fresh draft: the leftover cursor is
        // not resumed, and its draft is not walked over.
        assert!(!history.is_browsing(Some(&second)));
        assert_eq!(
            history.previous(&second, &entries, "a draft"),
            HistoryStep::Ignored
        );
    }

    #[test]
    fn history_gives_up_a_cursor_whose_message_left_the_timeline() {
        let session = vibex_core::VibexSessionId::new();
        let mut history = ComposerHistory::default();
        history.begin(&session, "");
        history.previous(&session, &["one".to_string(), "two".to_string()], "");
        // The session was cleared: the cursor names a row that no longer
        // exists, so the walk ends rather than pointing at nothing.
        assert_eq!(history.previous(&session, &[], ""), HistoryStep::Ignored);
        assert!(!history.is_browsing(Some(&session)));
    }

    #[test]
    fn cancelling_a_walk_hands_the_draft_back_once() {
        let session = vibex_core::VibexSessionId::new();
        let entries = vec!["sent".to_string()];
        let mut history = ComposerHistory::default();
        assert_eq!(history.cancel(), None, "there was no walk to leave");
        history.begin(&session, "my draft");
        history.previous(&session, &entries, "my draft");
        assert_eq!(history.cancel().as_deref(), Some("my draft"));
        assert_eq!(history.cancel(), None, "the draft came back twice");
    }

    #[test]
    fn the_sent_log_is_bounded_and_deduplicated() {
        let mut log = ComposerLog::new(3);
        for index in 0..5 {
            log.push(format!("entry {index}"));
        }
        assert_eq!(log.entries().len(), 3);
        assert_eq!(log.entries()[0], "entry 2");
        log.push("entry 4");
        assert_eq!(log.entries().len(), 3);
        assert_eq!(log.entries().last().unwrap(), "entry 4");
    }

    #[test]
    fn display_lines_mark_the_cursor_line_and_wrap() {
        let mut draft = buffer("short\n这是一段很长的中文文本需要换行");
        let lines = draft.display_lines(10);
        assert!(lines.len() >= 3);
        for (text, _) in &lines {
            // A row may carry the whitespace its break was decided on, which is
            // invisible; what has to fit is the text the reader sees.
            assert!(display_width(text.trim_end()) <= 10, "{text:?}");
        }
        // The cursor starts at the end of the last logical line.
        assert!(lines.last().unwrap().1);
        draft.move_to_start();
        assert!(draft.display_lines(10).first().unwrap().1);
    }

    #[test]
    fn display_row_count_counts_wrapped_rows_not_newlines() {
        // The band is sized from this, so one long line has to report the rows
        // it will be drawn on rather than the one newline-separated line it is.
        let draft = buffer("word word word word word");
        assert_eq!(draft.line_count(), 1);
        assert_eq!(draft.display_row_count(9), 3);
        // The same text at a width it fits reports one row.
        assert_eq!(draft.display_row_count(80), 1);
        // A newline is a row of its own, including a trailing one.
        assert_eq!(buffer("a\nb").display_row_count(80), 2);
        assert_eq!(buffer("a\n").display_row_count(80), 2);
        // An empty draft still occupies the row the caret sits on.
        assert_eq!(buffer("").display_row_count(80), 1);
        // A width of zero is one column, never a division by zero.
        assert_eq!(buffer("abc").display_row_count(0), 3);
    }

    #[test]
    fn typing_a_space_moves_the_caret_and_the_space_lands() {
        // Regression: a row that ended at the last word dropped the space the
        // reader had just typed, so the caret did not move and nothing appeared
        // until the next word arrived.
        let mut draft = buffer("hello");
        draft.move_to_end();
        assert_eq!(draft.cursor_cell(80).1, 5);
        draft.insert_char(' ');
        assert_eq!(draft.cursor_cell(80).1, 6, "the caret follows the space");
        assert_eq!(draft.display_lines(80)[0].0, "hello ");
        draft.insert_char('x');
        assert_eq!(draft.cursor_cell(80).1, 7);
        assert_eq!(draft.display_lines(80)[0].0, "hello x");
    }

    #[test]
    fn a_caret_after_a_collapsed_space_run_does_not_panic() {
        // Regression: the composer wrapped with `wrap_text`, whose lines are
        // rendered text rather than source slices. A run of spaces before an
        // ideograph shifted every later offset, so placing the caret sliced the
        // row inside a multi-byte character and aborted the client.
        let mut draft = buffer("ab  中文");
        draft.move_to_end();
        // `ab  |中文`: the caret the draft reports sits between the two
        // ideographs, seven bytes in.
        draft.move_left();
        assert_eq!(draft.cursor(), 7);
        // Both the caret the renderer places and the click that maps back onto
        // it have to survive the offset. A frame lays the rows out first, which
        // is what records the width a click is mapped against.
        for width in 1..24 {
            draft.display_rows(width);
            let (row, column) = draft.cursor_cell(width);
            let mut clicked = draft.clone();
            clicked.move_cursor_to_cell(row, column);
            assert_eq!(clicked.cursor(), draft.cursor(), "width {width}");
        }
        draft.display_rows(10);
        assert_eq!(draft.cursor_cell(10).1, 6);
    }

    #[test]
    fn wrapped_draft_rows_are_the_slices_they_claim_to_be() {
        let mut draft = buffer("颜色太少了，你可以按  markdown 语法来选取强调色");
        for width in 1..30 {
            let rows = draft.display_rows(width);
            assert!(!rows.is_empty());
            for row in &rows {
                let source = &draft.text()[row.source_start..];
                assert!(
                    source.starts_with(&row.text),
                    "row {:?} is not the draft at {}",
                    row.text,
                    row.source_start
                );
            }
        }
    }

    #[test]
    fn completion_filtering_is_case_insensitive() {
        let menu = CompletionMenu {
            trigger: CompletionTrigger::Slash,
            start: 0,
            query: String::new(),
            lists: CompletionLists {
                commands: vec![
                    Completion {
                        insert: "/help".into(),
                        label: "help".into(),
                        detail: "Show help".into(),
                        group: "Commands".into(),
                    },
                    Completion {
                        insert: "/hooks".into(),
                        label: "hooks".into(),
                        detail: "Manage hooks".into(),
                        group: "Commands".into(),
                    },
                ],
                ..CompletionLists::default()
            },
            tab: CompletionTab::Commands,
            selected: 0,
            loading: false,
        };
        assert_eq!(menu.filtered("HE").len(), 1);
        assert_eq!(menu.filtered("").len(), 2);
        assert_eq!(menu.filtered("zzz").len(), 0);
    }

    #[test]
    fn a_long_paste_collapses_into_one_atomic_chip() {
        let mut buffer = ComposerBuffer::default();
        buffer.insert_str("here is the log: ");
        let log = (0..42)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(buffer.insert_paste(&log), "42 lines is a chip");
        assert_eq!(buffer.chip_count(), 1);
        // The draft shows the label, not the log.
        assert!(
            buffer.text().ends_with("[Pasted: 42 lines]"),
            "{}",
            buffer.text()
        );
        // One Backspace removes the whole paste.
        buffer.backspace();
        assert_eq!(buffer.text(), "here is the log: ");
        assert_eq!(buffer.chip_count(), 0);
        // ...and undo brings it back, still collapsed.
        assert!(buffer.undo());
        assert_eq!(buffer.chip_count(), 1);
    }

    #[test]
    fn a_short_paste_stays_literal() {
        let mut buffer = ComposerBuffer::default();
        assert!(!buffer.insert_paste("two\nlines"));
        assert_eq!(buffer.chip_count(), 0);
        assert_eq!(buffer.text(), "two\nlines");
    }

    #[test]
    fn the_chip_peek_reports_what_stands_under_the_caret() {
        let mut buffer = ComposerBuffer::default();
        let log = (0..9)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(buffer.insert_paste(&log));
        // A paste leaves the caret at the label's right edge, which is the
        // position the reader asks "what did I paste?" from.
        let peek = buffer.chip_peek().expect("the caret is at the chip");
        assert!(!peek.on_chip, "the caret is after the label, not on it");
        assert!(peek.is_paste());
        assert_eq!(peek.label, "[Pasted: 9 lines]");
        assert!(matches!(peek.payload, ChipPeekPayload::Paste(text) if text == log));

        // The label's left edge is the other position, and the one `Enter`
        // acts from: a chip is one stop, so a step left from its end lands on
        // its start rather than inside it.
        buffer.move_left();
        let peek = buffer.chip_peek().expect("the caret is on the chip");
        assert!(peek.on_chip, "the caret is at the label's first cell");

        // Away from the chip there is nothing to explain.
        buffer.move_to_end();
        buffer.insert_str(" and a trailing sentence");
        assert!(buffer.chip_peek().is_none());
    }

    #[test]
    fn enter_opens_a_paste_only_from_on_the_chip() {
        let mut buffer = ComposerBuffer::default();
        let log = (0..7)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(buffer.insert_paste(&log));
        // At the right edge the caret is at the end of a message, not on the
        // chip: the send that follows must not be turned into an expand.
        assert!(!buffer.expand_paste_at_cursor());
        assert_eq!(buffer.chip_count(), 1);

        // One step left is the label's own position, where `Enter` opens it.
        buffer.move_left();
        assert!(buffer.expand_paste_at_cursor());
        assert_eq!(buffer.text(), log);
        assert_eq!(buffer.chip_count(), 0);
    }

    #[test]
    fn an_image_chip_has_nothing_to_expand_and_says_so() {
        let mut buffer = ComposerBuffer::default();
        buffer.insert_image(attached_path("/tmp/shot.png"));
        buffer.move_left();
        let peek = buffer.chip_peek().expect("the caret is on the chip");
        assert!(peek.on_chip);
        assert!(!peek.is_paste());
        assert!(matches!(
            peek.payload,
            ChipPeekPayload::Image(image) if image.label == "[Image #1]"
        ));
        assert!(
            !buffer.expand_paste_at_cursor(),
            "an image has no text to open into the draft"
        );
        assert_eq!(buffer.chip_count(), 1);
    }

    #[test]
    fn a_chip_is_expanded_on_the_way_out_and_not_before() {
        let mut buffer = ComposerBuffer::default();
        let log = (0..10)
            .map(|index| format!("row {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        buffer.insert_paste(&log);
        assert!(!buffer.text().contains("row 3"));
        let sent = buffer.expanded_text();
        assert!(sent.contains("row 3"), "{sent}");
        assert_eq!(sent, log);
        // Sending clears the chip with the draft.
        let taken = buffer.take_expanded();
        assert_eq!(taken, log);
        assert!(buffer.text().is_empty());
        assert_eq!(buffer.chip_count(), 0);
    }

    #[test]
    fn repasting_the_same_content_expands_the_chip_instead_of_duplicating_it() {
        let mut buffer = ComposerBuffer::default();
        let log = (0..8)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        buffer.insert_paste(&log);
        assert_eq!(buffer.chip_count(), 1);
        buffer.move_to_end();
        assert!(!buffer.insert_paste(&log), "the second paste expands");
        assert_eq!(buffer.chip_count(), 0);
        assert_eq!(buffer.text(), log);
    }

    #[test]
    fn editing_a_chip_dissolves_it_rather_than_leaving_a_stale_range() {
        let mut buffer = ComposerBuffer::default();
        let log = (0..6)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        buffer.insert_paste(&log);
        assert_eq!(buffer.chip_count(), 1);
        // Backspace twice: once removes the chip, the second edits the draft.
        buffer.backspace();
        assert_eq!(buffer.chip_count(), 0);
        buffer.insert_str("plain");
        assert_eq!(buffer.expanded_text(), "plain");
    }

    #[test]
    fn the_cursor_steps_over_a_chip_whole() {
        let mut buffer = ComposerBuffer::default();
        let log = (0..5)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        buffer.insert_paste(&log);
        let end = buffer.cursor();
        buffer.move_left();
        assert_eq!(buffer.cursor(), 0, "one step, not one per label character");
        buffer.move_right();
        assert_eq!(buffer.cursor(), end);
    }

    #[test]
    fn line_breaks_are_normalised_before_a_chip_is_measured() {
        assert_eq!(normalize_line_breaks("a\r\nb\rc\u{2028}d"), "a\nb\nc\nd");
        let mut buffer = ComposerBuffer::default();
        // Four CRLF lines are four lines, so this is a chip.
        assert!(buffer.insert_paste("1\r\n2\r\n3\r\n4"));
        assert_eq!(buffer.chip_count(), 1);
    }

    #[test]
    fn an_empty_draft_is_reported_as_empty() {
        let mut buffer = ComposerBuffer::default();
        assert!(buffer.is_empty());
        buffer.insert_str("   \n  ");
        assert!(buffer.is_empty());
        buffer.insert_str("x");
        assert!(!buffer.is_empty());
    }

    #[test]
    fn typing_coalesces_into_one_undo_step_per_word() {
        let mut buffer = ComposerBuffer::default();
        for character in "fix the".chars() {
            buffer.insert_char(character);
        }
        assert!(buffer.undo());
        // One step back lands before the last word, not before its last letter.
        assert_eq!(buffer.text(), "fix ");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "fix");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "");
        assert!(!buffer.undo(), "the empty draft is the floor");
    }

    #[test]
    fn a_cursor_move_ends_the_typing_batch() {
        let mut buffer = ComposerBuffer::default();
        buffer.insert_str("abc");
        buffer.insert_char('d');
        buffer.insert_char('e');
        buffer.move_left();
        buffer.insert_char('f');
        assert!(buffer.undo());
        // Only the character typed after the move is undone: the cursor was
        // placed deliberately, and undo must not move the text out from under
        // it.
        assert_eq!(buffer.text(), "abcde");
    }

    #[test]
    fn redo_replays_what_undo_took_back() {
        let mut buffer = ComposerBuffer::default();
        buffer.insert_str("first");
        buffer.insert_str(" second");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "first");
        assert!(buffer.redo());
        assert_eq!(buffer.text(), "first second");
        assert!(!buffer.redo());
        // A fresh edit drops the redo tail rather than replaying onto it.
        buffer.undo();
        buffer.insert_char('!');
        assert!(!buffer.can_redo());
    }

    #[test]
    fn a_cleared_draft_can_be_undone() {
        let mut buffer = ComposerBuffer::from_text("a paragraph worth keeping");
        buffer.clear();
        assert_eq!(buffer.text(), "");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "a paragraph worth keeping");
    }

    #[test]
    fn kill_and_yank_round_trip() {
        let mut buffer = ComposerBuffer::from_text("keep this tail");
        buffer.move_to_start();
        for _ in 0..5 {
            buffer.move_right();
        }
        assert!(buffer.kill_to_line_end());
        assert_eq!(buffer.text(), "keep ");
        assert_eq!(buffer.kill_buffer(), "this tail");
        buffer.move_to_end();
        assert!(buffer.yank());
        assert_eq!(buffer.text(), "keep this tail");

        // The line-start kill is the mirror image, and the draft can be brought
        // back from it.
        buffer.move_to_end();
        assert!(buffer.kill_to_line_start());
        assert_eq!(buffer.text(), "");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "keep this tail");
    }

    #[test]
    fn word_motions_stop_at_class_boundaries() {
        let mut buffer = ComposerBuffer::from_text("read src/net.rs now");
        buffer.move_to_start();
        buffer.move_word_right();
        // The cursor lands at the end of `read`, before the space.
        assert_eq!(buffer.cursor(), 4);
        buffer.move_word_right();
        assert_eq!(&buffer.text()[..buffer.cursor()], "read src");
        buffer.move_word_right();
        assert_eq!(&buffer.text()[..buffer.cursor()], "read src/");
        buffer.move_word_right();
        assert_eq!(&buffer.text()[..buffer.cursor()], "read src/net");
        // Backwards from the end retraces the same stops.
        buffer.move_to_end();
        buffer.move_word_left();
        assert_eq!(&buffer.text()[buffer.cursor()..], "now");
        buffer.move_word_left();
        assert_eq!(&buffer.text()[buffer.cursor()..], "rs now");
    }

    #[test]
    fn forward_and_backward_word_kills_agree_with_the_motions() {
        let mut buffer = ComposerBuffer::from_text("read src/net.rs now");
        buffer.move_to_start();
        buffer.move_word_right();
        buffer.move_word_right();
        // Sitting between `src` and `/`, a forward kill takes the separator.
        assert!(buffer.delete_word_after());
        assert_eq!(buffer.text(), "read srcnet.rs now");
        assert_eq!(buffer.kill_buffer(), "/");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "read src/net.rs now");

        // From the end, a backward kill takes the trailing word.
        buffer.move_to_end();
        assert!(buffer.delete_word_backward());
        assert_eq!(buffer.text(), "read src/net.rs ");
        assert_eq!(buffer.kill_buffer(), "now");
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut buffer = ComposerBuffer::from_text("hello world");
        buffer.move_to_start();
        for _ in 0..5 {
            buffer.extend_right();
        }
        assert_eq!(buffer.selection(), Some((0, 5)));
        assert_eq!(buffer.selected_text().as_deref(), Some("hello"));
        buffer.insert_char('X');
        assert_eq!(buffer.text(), "X world");
        assert_eq!(buffer.selection(), None);
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "hello world");
    }

    #[test]
    fn backspace_removes_the_whole_selection() {
        let mut buffer = ComposerBuffer::from_text("keep this tail");
        buffer.move_to_start();
        for _ in 0..5 {
            buffer.extend_right();
        }
        assert!(buffer.backspace());
        assert_eq!(buffer.text(), "this tail");
        assert_eq!(buffer.cursor(), 0);
        assert_eq!(buffer.selection(), None);
    }

    #[test]
    fn a_selection_widens_over_a_collapsed_paste() {
        let mut buffer = ComposerBuffer::default();
        buffer.insert_str("before ");
        let content = (1..=6)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(buffer.insert_paste(&content));
        buffer.insert_str(" after");
        let (chip_start, chip_end) = buffer.chip_ranges()[0];
        assert_eq!(&buffer.text()[chip_start..chip_end], "[Pasted: 6 lines]");

        // A pointer that lands in the middle of the label still selects all of
        // it, so a cut can never leave half a marker behind.
        buffer.move_cursor_to_cell(0, chip_start as u16 + 3);
        buffer.begin_selection();
        buffer.extend_selection_to_cell(0, 0);
        assert_eq!(buffer.selection(), Some((0, chip_end)));
        assert_eq!(buffer.chip_count(), 1);
        assert!(buffer.backspace());
        assert_eq!(buffer.chip_count(), 0);
        // The prose before the chip and the label go together; what followed
        // the chip stays.
        assert_eq!(buffer.text(), " after");
    }

    #[test]
    fn a_selection_made_by_a_click_alone_covers_nothing() {
        let mut buffer = ComposerBuffer::from_text("a draft");
        buffer.move_to_end();
        buffer.begin_selection();
        assert_eq!(buffer.selection(), None);
        assert!(!buffer.clear_selection());
    }

    #[test]
    fn select_all_covers_the_draft_and_a_kill_takes_it_expanded() {
        let mut buffer = ComposerBuffer::default();
        buffer.insert_str("run ");
        let content = "alpha\nbeta\ngamma\ndelta";
        assert!(buffer.insert_paste(content));
        buffer.select_all();
        assert_eq!(
            buffer.selected_text().as_deref(),
            Some("run [Pasted: 4 lines]")
        );
        // Cutting a chip takes what the chip stands for, not its label, so a
        // yank puts the content back.
        assert!(buffer.delete_word_after());
        assert_eq!(buffer.kill_buffer(), format!("run {content}"));
        assert_eq!(buffer.text(), "");
        assert!(buffer.yank());
        assert_eq!(buffer.text(), format!("run {content}"));
    }

    #[test]
    fn a_plain_motion_drops_the_highlight_but_an_extended_one_keeps_it() {
        let mut buffer = ComposerBuffer::from_text("one two");
        buffer.move_to_start();
        buffer.extend_right();
        buffer.extend_right();
        assert_eq!(buffer.selection(), Some((0, 2)));
        buffer.move_right();
        assert_eq!(buffer.selection(), None);

        // Extending after a mouse selection keeps the same anchor.
        buffer.begin_selection();
        buffer.move_to_start();
        buffer.extend_word_right();
        assert_eq!(buffer.selection(), Some((0, 3)));
        buffer.extend_word_right();
        assert_eq!(buffer.selection(), Some((0, 7)));
    }

    /// The bytes a clipboard image would arrive with.
    fn png_bytes() -> Vec<u8> {
        vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
    }

    /// An attachment as the client's paste path builds one: a path, its length
    /// and nothing decoded.
    fn attached_path(path: &str) -> AttachedImage {
        AttachedImage {
            mime_type: "image/png".into(),
            byte_len: 0,
            thumbnail: None,
            source: ImageSource::Path(path.to_string()),
        }
    }

    #[test]
    fn an_image_attaches_as_a_chip_and_leaves_the_text() {
        let mut buffer = ComposerBuffer::default();
        buffer.insert_str("look at this ");
        let label = buffer
            .insert_image(AttachedImage {
                mime_type: "image/png".into(),
                source: ImageSource::Bytes(std::sync::Arc::new(png_bytes())),
                byte_len: png_bytes().len(),
                thumbnail: None,
            })
            .expect("the first image fits");
        assert_eq!(label, "[Image #1]");
        assert_eq!(buffer.text(), "look at this [Image #1]");
        assert_eq!(buffer.image_count(), 1);
        // The label is a placeholder: what the Agent receives is the picture.
        assert_eq!(buffer.expanded_text(), "look at this ");
        let images = buffer.images();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].label, "[Image #1]");
        assert_eq!(images[0].mime_type, "image/png");
    }

    #[test]
    fn an_image_chip_is_one_object() {
        let mut buffer = ComposerBuffer::default();
        buffer.insert_str("before ");
        buffer.insert_image(attached_path("/tmp/a.png"));
        assert_eq!(buffer.text(), "before [Image #1]");
        // The cursor sits just past the label, so one press removes all of it.
        assert!(buffer.backspace());
        assert_eq!(buffer.text(), "before ");
        assert_eq!(buffer.image_count(), 0);
    }

    #[test]
    fn a_draft_taken_for_sending_carries_its_images() {
        let mut buffer = ComposerBuffer::default();
        buffer.insert_str("see ");
        buffer.insert_image(attached_path("/tmp/shot.png"));
        let (text, images) = buffer.take_with_attachments();
        assert_eq!(text, "see ");
        assert_eq!(images.len(), 1);
        assert!(buffer.text().is_empty());
        assert_eq!(buffer.image_count(), 0);
    }

    #[test]
    fn the_image_cap_refuses_the_eleventh() {
        let mut buffer = ComposerBuffer::default();
        for index in 0..IMAGE_CAP {
            assert!(
                buffer.insert_image(attached_path("/tmp/a.png")).is_some(),
                "image {index} was refused below the cap"
            );
        }
        assert!(buffer.image_count() == IMAGE_CAP);
        assert!(
            buffer.insert_image(attached_path("/tmp/a.png")).is_none(),
            "the cap did not hold"
        );
    }

    #[test]
    fn image_labels_are_not_recycled_within_a_draft() {
        let mut buffer = ComposerBuffer::default();
        assert_eq!(
            buffer.insert_image(attached_path("/tmp/a.png")),
            Some("[Image #1]".to_string())
        );
        buffer.move_to_start();
        assert!(buffer.delete());
        assert_eq!(buffer.image_count(), 0);
        // The second image is #2: a number that once named a picture must never
        // name a different one.
        assert_eq!(
            buffer.insert_image(attached_path("/tmp/b.png")),
            Some("[Image #2]".to_string())
        );
        // Emptied, the draft starts counting again.
        buffer.take_expanded();
        assert_eq!(
            buffer.insert_image(attached_path("/tmp/c.png")),
            Some("[Image #1]".to_string())
        );
    }

    #[test]
    fn the_wire_form_of_an_image_names_a_file_or_carries_the_bytes() {
        // A path becomes a `file://` URI and an absolute one, because the
        // client that draws it may have a different working directory.
        let path = message_attachment(
            &ImageAttachment {
                label: "[Image #1]".into(),
                mime_type: "image/png".into(),
                source: ImageSource::Path("/tmp/shot.png".into()),
                byte_len: 0,
                thumbnail: None,
            },
            7,
            true,
        );
        assert_eq!(path.uri.as_deref(), Some("file:///tmp/shot.png"));
        assert_eq!(path.mime_type.as_deref(), Some("image/png"));
        assert_eq!(path.label, "[Image #1]");
        assert_eq!(
            path.inline_text_offset,
            Some(7),
            "the place in the message is what stops the picture moving to the end"
        );

        // A relative path is resolved against the client's directory.
        let relative = message_attachment(
            &ImageAttachment {
                label: "[Image #2]".into(),
                mime_type: "image/png".into(),
                source: ImageSource::Path("shot.png".into()),
                byte_len: 0,
                thumbnail: None,
            },
            0,
            true,
        );
        let uri = relative.uri.expect("a uri");
        assert!(uri.starts_with("file:///"), "{uri}");
        assert!(uri.ends_with("/shot.png"), "{uri}");

        // Clipboard bytes on a remote seat travel with the message.
        let remote = message_attachment(
            &ImageAttachment {
                label: "[Image #3]".into(),
                mime_type: "image/png".into(),
                source: ImageSource::Bytes(std::sync::Arc::new(png_bytes())),
                byte_len: 0,
                thumbnail: None,
            },
            0,
            false,
        );
        assert_eq!(
            remote.uri.as_deref(),
            Some("data:image/png;base64,iVBORw0KGgo=")
        );

        // On the authority they become a file the desktop can draw.
        let local = message_attachment(
            &ImageAttachment {
                label: "[Image #4]".into(),
                mime_type: "image/png".into(),
                source: ImageSource::Bytes(std::sync::Arc::new(png_bytes())),
                byte_len: 0,
                thumbnail: None,
            },
            0,
            true,
        );
        let uri = local.uri.expect("a uri");
        let path = uri.strip_prefix("file://").expect("a file uri");
        assert_eq!(
            std::fs::read(path).expect("the bytes were written"),
            png_bytes()
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_image_carries_the_offset_it_sat_at() {
        // The offset is in the text as it is *sent*: a paste expanded in front
        // of an image moves the image, and a double-width character counts as
        // what the wire counts it as.
        let mut buffer = ComposerBuffer::default();
        buffer.insert_str("前面 ");
        buffer
            .insert_image(attached_path("/tmp/a.png"))
            .expect("the image attaches");
        buffer.insert_str(" 后面");
        let outgoing = buffer.outgoing();
        assert_eq!(outgoing.text, "前面  后面");
        assert_eq!(outgoing.images.len(), 1);
        assert_eq!(
            outgoing.images[0].1, 3,
            "the offset counts UTF-16 units: {:?}",
            outgoing.text
        );

        // An image at the end of the message is still an offset, not a guess.
        let mut trailing = ComposerBuffer::default();
        trailing.insert_str("hi ");
        trailing
            .insert_image(attached_path("/tmp/b.png"))
            .expect("the image attaches");
        assert_eq!(trailing.outgoing().images[0].1, 3);
    }

    #[test]
    fn restoring_a_draft_puts_its_images_back_where_they_were() {
        let mut buffer = ComposerBuffer::default();
        let image = ImageAttachment {
            label: "[Image #1]".into(),
            mime_type: "image/png".into(),
            source: ImageSource::Path("/tmp/shot.png".into()),
            byte_len: 0,
            thumbnail: None,
        };
        // The offset is where the picture sat in the text as it was sent; the
        // label goes back there, not at the end of the paragraph.
        buffer.set_draft("look at  then continue", vec![(image.clone(), 8)]);
        assert_eq!(buffer.text(), "look at [Image #1] then continue");
        assert_eq!(buffer.image_count(), 1);

        // Two images keep their order and their places.
        let mut second = image.clone();
        second.label = "[Image #2]".into();
        let mut pair = ComposerBuffer::default();
        pair.set_draft("a  b  c", vec![(second, 5), (image, 2)]);
        assert_eq!(pair.text(), "a [Image #1] b [Image #2] c");
        assert_eq!(pair.image_count(), 2);
    }

    #[test]
    fn only_an_image_extension_names_an_image() {
        assert_eq!(image_mime_for_path("/tmp/a.PNG"), Some("image/png"));
        assert_eq!(image_mime_for_path("/tmp/a.jpeg"), Some("image/jpeg"));
        assert_eq!(image_mime_for_path("/tmp/a.txt"), None);
        assert_eq!(image_mime_for_path("/tmp/a"), None);
    }

    #[test]
    fn extending_into_a_line_break_selects_it() {
        let mut buffer = ComposerBuffer::from_text("one\ntwo");
        buffer.move_to_start();
        buffer.extend_line_end();
        assert_eq!(buffer.selection(), Some((0, 3)));
        buffer.extend_right();
        assert_eq!(buffer.selection(), Some((0, 4)));
        assert_eq!(buffer.selected_text().as_deref(), Some("one\n"));
    }
}

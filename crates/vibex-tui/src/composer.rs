//! The message composer: a multi-line grapheme-correct buffer with history and
//! trigger-driven completion.
//!
//! The composer owns no product semantics. `/`, `@` and `$` are *triggers* that
//! ask the authority what they mean (`discover_agent_commands`); this module
//! only knows how to detect a trigger, present the returned candidates and
//! splice an accepted candidate back into the text.

use unicode_segmentation::UnicodeSegmentation;

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

/// The open completion menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionMenu {
    pub trigger: CompletionTrigger,
    /// Byte range of the trigger word in the buffer.
    pub start: usize,
    pub end: usize,
    pub items: Vec<Completion>,
    pub selected: usize,
    pub loading: bool,
}

/// How many rows the drawer shows before it scrolls.
pub const MAX_VISIBLE_COMPLETIONS: usize = 8;

impl CompletionMenu {
    /// The rows to draw: the filtered matches, windowed around the selection.
    ///
    /// The menu carries no query of its own -- the composer owns the text -- so
    /// the caller filters and this windows the result, which keeps the drawer
    /// showing the selected row even after the list has scrolled.
    pub fn visible(&self) -> Vec<usize> {
        let all = (0..self.items.len()).collect::<Vec<_>>();
        if all.len() <= MAX_VISIBLE_COMPLETIONS {
            return all;
        }
        let half = MAX_VISIBLE_COMPLETIONS / 2;
        let start = self
            .selected
            .saturating_sub(half)
            .min(all.len() - MAX_VISIBLE_COMPLETIONS);
        all[start..start + MAX_VISIBLE_COMPLETIONS].to_vec()
    }

    pub fn filtered(&self, query: &str) -> Vec<usize> {
        let query = query.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                query.is_empty()
                    || item.label.to_lowercase().contains(&query)
                    || item.detail.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
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

/// An image attached to the draft, before it is turned into a wire attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAttachment {
    /// `Image #1`, exactly as the label reads.
    pub label: String,
    pub mime_type: String,
    pub source: ImageSource,
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
    pub fn insert_image(
        &mut self,
        mime_type: impl Into<String>,
        source: ImageSource,
    ) -> Option<String> {
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
                mime_type: mime_type.into(),
                source,
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
            if let Some(label) = self.insert_image(image.mime_type, image.source) {
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
    /// `and/or` is not a command, and neither is the absolute path in
    /// `cat /etc/hosts`: a slash command must be the first token on its line.
    /// `@` and `$` may appear anywhere a word can start.
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
        if trigger == CompletionTrigger::Slash {
            let line_start = self.text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
            if !self.text[line_start..start]
                .chars()
                .all(char::is_whitespace)
            {
                return None;
            }
        }
        Some((trigger, start, characters.collect()))
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
fn materialise_attachment(mime_type: &str, bytes: &[u8]) -> Option<String> {
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

/// Sent-message history with a cursor into it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposerHistory {
    entries: Vec<String>,
    /// `None` means "editing a fresh draft"; `Some(index)` walks backwards.
    index: Option<usize>,
    draft: String,
    limit: usize,
}

impl ComposerHistory {
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            ..Self::default()
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
        self.reset();
    }

    pub fn reset(&mut self) {
        self.index = None;
        self.draft.clear();
    }

    /// Step to an older entry. Returns the text to load, if any.
    pub fn previous(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.index {
            None => {
                self.draft = current.to_string();
                self.entries.len() - 1
            }
            Some(0) => return None,
            Some(index) => index - 1,
        };
        self.index = Some(next);
        Some(self.entries[next].clone())
    }

    /// Step to a newer entry, restoring the draft at the end.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<String> {
        let index = self.index?;
        if index + 1 >= self.entries.len() {
            self.index = None;
            return Some(std::mem::take(&mut self.draft));
        }
        self.index = Some(index + 1);
        Some(self.entries[index + 1].clone())
    }

    pub fn is_browsing(&self) -> bool {
        self.index.is_some()
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
fn compact_bytes(bytes: usize) -> String {
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

        // An absolute path is not a command.
        let draft = buffer("cat /etc/hosts");
        assert_eq!(draft.active_trigger(), None);
    }

    #[test]
    fn accepting_a_completion_replaces_the_trigger_word() {
        let mut draft = buffer("/he");
        let (_, start, _) = draft.active_trigger().unwrap();
        draft.replace_trigger_word(start, "/help ");
        assert_eq!(draft.text(), "/help ");
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
        let mut history = ComposerHistory::new(10);
        history.push("first");
        history.push("second");
        assert_eq!(history.previous("my draft").as_deref(), Some("second"));
        assert_eq!(history.previous("").as_deref(), Some("first"));
        assert_eq!(history.previous(""), None);
        assert_eq!(history.next().as_deref(), Some("second"));
        assert_eq!(history.next().as_deref(), Some("my draft"));
        assert!(!history.is_browsing());
    }

    #[test]
    fn history_is_bounded_and_deduplicated() {
        let mut history = ComposerHistory::new(3);
        for index in 0..5 {
            history.push(format!("entry {index}"));
        }
        assert_eq!(history.entries().len(), 3);
        assert_eq!(history.entries()[0], "entry 2");
        history.push("entry 4");
        assert_eq!(history.entries().len(), 3);
        assert_eq!(history.entries().last().unwrap(), "entry 4");
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
            end: 1,
            items: vec![
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

    #[test]
    fn an_image_attaches_as_a_chip_and_leaves_the_text() {
        let mut buffer = ComposerBuffer::default();
        buffer.insert_str("look at this ");
        let label = buffer
            .insert_image(
                "image/png",
                ImageSource::Bytes(std::sync::Arc::new(png_bytes())),
            )
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
        buffer.insert_image("image/png", ImageSource::Path("/tmp/a.png".into()));
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
        buffer.insert_image("image/png", ImageSource::Path("/tmp/shot.png".into()));
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
                buffer
                    .insert_image("image/png", ImageSource::Path("/tmp/a.png".into()))
                    .is_some(),
                "image {index} was refused below the cap"
            );
        }
        assert!(buffer.image_count() == IMAGE_CAP);
        assert!(
            buffer
                .insert_image("image/png", ImageSource::Path("/tmp/a.png".into()))
                .is_none(),
            "the cap did not hold"
        );
    }

    #[test]
    fn image_labels_are_not_recycled_within_a_draft() {
        let mut buffer = ComposerBuffer::default();
        assert_eq!(
            buffer.insert_image("image/png", ImageSource::Path("/tmp/a.png".into())),
            Some("[Image #1]".to_string())
        );
        buffer.move_to_start();
        assert!(buffer.delete());
        assert_eq!(buffer.image_count(), 0);
        // The second image is #2: a number that once named a picture must never
        // name a different one.
        assert_eq!(
            buffer.insert_image("image/png", ImageSource::Path("/tmp/b.png".into())),
            Some("[Image #2]".to_string())
        );
        // Emptied, the draft starts counting again.
        buffer.take_expanded();
        assert_eq!(
            buffer.insert_image("image/png", ImageSource::Path("/tmp/c.png".into())),
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
            .insert_image("image/png", ImageSource::Path("/tmp/a.png".into()))
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
            .insert_image("image/png", ImageSource::Path("/tmp/b.png".into()))
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

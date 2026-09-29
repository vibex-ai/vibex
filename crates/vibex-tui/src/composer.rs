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

impl CompletionMenu {
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

/// A multi-line editable buffer with a grapheme-aligned cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposerBuffer {
    text: String,
    /// Cursor position as a byte offset, always on a grapheme boundary.
    cursor: usize,
    /// Desired column, preserved while moving vertically through short lines.
    preferred_column: Option<usize>,
}

impl ComposerBuffer {
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self {
            text,
            cursor,
            preferred_column: None,
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

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.preferred_column = None;
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
        self.preferred_column = None;
    }

    /// Take the text out, leaving an empty buffer.
    pub fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.preferred_column = None;
        text
    }

    pub fn insert_char(&mut self, character: char) {
        self.text.insert(self.cursor, character);
        self.cursor += character.len_utf8();
        self.preferred_column = None;
    }

    pub fn insert_str(&mut self, value: &str) {
        self.text.insert_str(self.cursor, value);
        self.cursor += value.len();
        self.preferred_column = None;
    }

    /// Delete the grapheme before the cursor.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let start = self.previous_grapheme_boundary();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.preferred_column = None;
        true
    }

    /// Delete the grapheme under the cursor.
    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.text.len() {
            return false;
        }
        let end = self.next_grapheme_boundary();
        self.text.replace_range(self.cursor..end, "");
        self.preferred_column = None;
        true
    }

    /// Delete the word before the cursor, the way a shell's `Ctrl+W` does.
    pub fn delete_word_before(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let mut start = self.cursor;
        // Skip trailing whitespace, then the word.
        while start > 0 {
            let previous = self.prev_boundary_from(start);
            let grapheme = &self.text[previous..start];
            if grapheme.chars().all(char::is_whitespace) {
                start = previous;
            } else {
                break;
            }
        }
        while start > 0 {
            let previous = self.prev_boundary_from(start);
            let grapheme = &self.text[previous..start];
            if grapheme.chars().all(char::is_whitespace) {
                break;
            }
            start = previous;
        }
        if start == self.cursor {
            return false;
        }
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.preferred_column = None;
        true
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
        self.cursor = self.previous_grapheme_boundary();
        self.preferred_column = None;
    }

    pub fn move_right(&mut self) {
        self.cursor = self.next_grapheme_boundary();
        self.preferred_column = None;
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
        let (line, _) = self.cursor_line_column();
        self.cursor = self.line_range(line).0;
        self.preferred_column = None;
    }

    pub fn move_line_end(&mut self) {
        let (line, _) = self.cursor_line_column();
        self.cursor = self.line_range(line).1;
        self.preferred_column = None;
    }

    pub fn move_to_start(&mut self) {
        self.cursor = 0;
        self.preferred_column = None;
    }

    pub fn move_to_end(&mut self) {
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

    /// The visual lines of the buffer, wrapped to `width`.
    pub fn display_lines(&self, width: usize) -> Vec<(String, bool)> {
        let (cursor_line, _) = self.cursor_line_column();
        let mut out = Vec::new();
        for line in 0..self.line_count() {
            let (start, end) = self.line_range(line);
            let text = &self.text[start..end];
            let wrapped = crate::text::wrap_text(text, width.max(1));
            for segment in wrapped {
                out.push((segment.text, line == cursor_line));
            }
        }
        if out.is_empty() {
            out.push((String::new(), true));
        }
        out
    }
}

fn byte_offset_for_column(line: &str, column: usize) -> usize {
    if column == 0 {
        return 0;
    }
    let (prefix, _) = take_width(line, column);
    prefix.len()
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
            assert!(display_width(text) <= 10, "{text:?}");
        }
        // The cursor starts at the end of the last logical line.
        assert!(lines.last().unwrap().1);
        draft.move_to_start();
        assert!(draft.display_lines(10).first().unwrap().1);
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
    fn an_empty_draft_is_reported_as_empty() {
        let mut buffer = ComposerBuffer::default();
        assert!(buffer.is_empty());
        buffer.insert_str("   \n  ");
        assert!(buffer.is_empty());
        buffer.insert_str("x");
        assert!(!buffer.is_empty());
    }
}

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const DIFF_MAX_ROWS: usize = 100_000;
pub const DIFF_DEFAULT_OVERSCAN: usize = 40;
pub const DIFF_WORD_CACHE_ROWS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnifiedDiffLineKind {
    Meta,
    Hunk,
    Add,
    Delete,
    Context,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnifiedDiffLine {
    pub kind: UnifiedDiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnifiedDiffFile {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub header: Vec<String>,
    pub lines: Vec<UnifiedDiffLine>,
    pub binary: bool,
    pub renamed: bool,
    pub copied: bool,
}

impl UnifiedDiffFile {
    pub fn display_path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("diff")
    }
}

pub fn parse_unified_diff(diff: &str) -> Vec<UnifiedDiffFile> {
    let mut files = Vec::<UnifiedDiffFile>::new();
    let mut current = None::<usize>;
    let mut old_line = None::<u32>;
    let mut new_line = None::<u32>;

    for raw_line in diff.split_terminator('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let (old_path, new_path) = parse_git_diff_paths(rest);
            files.push(UnifiedDiffFile {
                old_path,
                new_path,
                header: vec![line.to_string()],
                lines: Vec::new(),
                binary: false,
                renamed: false,
                copied: false,
            });
            current = Some(files.len() - 1);
            old_line = None;
            new_line = None;
            continue;
        }

        let index = *current.get_or_insert_with(|| {
            files.push(UnifiedDiffFile {
                old_path: None,
                new_path: None,
                header: Vec::new(),
                lines: Vec::new(),
                binary: false,
                renamed: false,
                copied: false,
            });
            files.len() - 1
        });
        let file = &mut files[index];

        if let Some(path) = line.strip_prefix("--- ") {
            file.old_path = normalize_diff_marker_path(path);
            file.header.push(line.to_string());
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            file.new_path = normalize_diff_marker_path(path);
            file.header.push(line.to_string());
            continue;
        }
        if let Some(path) = line.strip_prefix("rename from ") {
            file.old_path = decode_git_path(path).or_else(|| Some(path.to_string()));
            file.renamed = true;
            file.header.push(line.to_string());
            continue;
        }
        if let Some(path) = line.strip_prefix("rename to ") {
            file.new_path = decode_git_path(path).or_else(|| Some(path.to_string()));
            file.renamed = true;
            file.header.push(line.to_string());
            continue;
        }
        if let Some(path) = line.strip_prefix("copy from ") {
            file.old_path = decode_git_path(path).or_else(|| Some(path.to_string()));
            file.copied = true;
            file.header.push(line.to_string());
            continue;
        }
        if let Some(path) = line.strip_prefix("copy to ") {
            file.new_path = decode_git_path(path).or_else(|| Some(path.to_string()));
            file.copied = true;
            file.header.push(line.to_string());
            continue;
        }
        if line.starts_with("Binary files ")
            || line.starts_with("GIT binary patch")
            || line.starts_with("Binary file ")
        {
            file.binary = true;
            file.header.push(line.to_string());
            old_line = None;
            new_line = None;
            continue;
        }
        if line.starts_with("@@ ") {
            let Some((next_old_line, next_new_line)) = parse_hunk_header(line) else {
                old_line = None;
                new_line = None;
                file.header.push(line.to_string());
                continue;
            };
            old_line = Some(next_old_line);
            new_line = Some(next_new_line);
            file.lines.push(UnifiedDiffLine {
                kind: UnifiedDiffLineKind::Hunk,
                old_line: None,
                new_line: None,
                content: line.to_string(),
            });
            continue;
        }
        if old_line.is_none() || new_line.is_none() {
            if !line.is_empty() {
                file.header.push(line.to_string());
            }
            continue;
        }
        if let Some(content) = line.strip_prefix('+') {
            file.lines.push(UnifiedDiffLine {
                kind: UnifiedDiffLineKind::Add,
                old_line: None,
                new_line,
                content: content.to_string(),
            });
            new_line = new_line.and_then(|line| line.checked_add(1));
        } else if let Some(content) = line.strip_prefix('-') {
            file.lines.push(UnifiedDiffLine {
                kind: UnifiedDiffLineKind::Delete,
                old_line,
                new_line: None,
                content: content.to_string(),
            });
            old_line = old_line.and_then(|line| line.checked_add(1));
        } else if line.starts_with('\\') {
            file.lines.push(UnifiedDiffLine {
                kind: UnifiedDiffLineKind::Meta,
                old_line: None,
                new_line: None,
                content: line.to_string(),
            });
        } else {
            let content = line.strip_prefix(' ').unwrap_or(line);
            file.lines.push(UnifiedDiffLine {
                kind: UnifiedDiffLineKind::Context,
                old_line,
                new_line,
                content: content.to_string(),
            });
            old_line = old_line.and_then(|line| line.checked_add(1));
            new_line = new_line.and_then(|line| line.checked_add(1));
        }
    }

    files
        .into_iter()
        .filter(|file| {
            file.old_path.is_some()
                || file.new_path.is_some()
                || !file.header.is_empty()
                || !file.lines.is_empty()
        })
        .collect()
}

fn parse_git_diff_paths(value: &str) -> (Option<String>, Option<String>) {
    let tokens = split_git_tokens(value);
    (
        tokens.first().and_then(|path| normalize_diff_path(path)),
        tokens.get(1).and_then(|path| normalize_diff_path(path)),
    )
}

fn split_git_tokens(value: &str) -> Vec<String> {
    let bytes = value.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        if bytes[index] == b'"' {
            let start = index;
            index += 1;
            let mut escaped = false;
            while index < bytes.len() {
                let byte = bytes[index];
                index += 1;
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    break;
                }
            }
            let token = &value[start..index];
            tokens.push(decode_git_path(token).unwrap_or_else(|| token.to_string()));
        } else {
            let start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            tokens.push(value[start..index].to_string());
        }
    }
    tokens
}

fn decode_git_path(value: &str) -> Option<String> {
    let value = value.trim();
    if !value.starts_with('"') {
        return Some(value.to_string());
    }
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    let bytes = inner.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        let byte = *bytes.get(index)?;
        index += 1;
        match byte {
            b'"' | b'\\' => output.push(byte),
            b'n' => output.push(b'\n'),
            b'r' => output.push(b'\r'),
            b't' => output.push(b'\t'),
            b'a' => output.push(0x07),
            b'b' => output.push(0x08),
            b'f' => output.push(0x0c),
            b'v' => output.push(0x0b),
            b'0'..=b'7' => {
                let mut value = u32::from(byte - b'0');
                for _ in 0..2 {
                    let Some(next @ b'0'..=b'7') = bytes.get(index).copied() else {
                        break;
                    };
                    value = value
                        .saturating_mul(8)
                        .saturating_add(u32::from(next - b'0'));
                    index += 1;
                }
                output.push(u8::try_from(value).ok()?);
            }
            _ => return None,
        }
    }
    String::from_utf8(output).ok()
}

fn normalize_diff_marker_path(value: &str) -> Option<String> {
    let path = value.split('\t').next().unwrap_or(value).trim();
    decode_git_path(path).and_then(|path| normalize_diff_path(&path))
}

fn normalize_diff_path(value: &str) -> Option<String> {
    let value = value.trim();
    if value == "/dev/null" {
        return Some(value.to_string());
    }
    let value = value
        .strip_prefix("a/")
        .or_else(|| value.strip_prefix("b/"))
        .unwrap_or(value);
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let body = line.strip_prefix("@@ -")?;
    let (old, body) = body.split_once(' ')?;
    let body = body.strip_prefix('+')?;
    let (new, _) = body.split_once(' ')?;
    Some((parse_range_start(old)?, parse_range_start(new)?))
}

fn parse_range_start(value: &str) -> Option<u32> {
    value.split(',').next()?.parse().ok()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffRow {
    pub id: String,
    pub file_index: usize,
    pub file_path: String,
    pub kind: UnifiedDiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordDiffKind {
    Equal,
    Added,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WordDiffSpan {
    pub kind: WordDiffKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedDiffRow {
    pub row: DiffRow,
    pub word_spans: Vec<WordDiffSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct WordCacheKey {
    revision: String,
    row_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WordCacheEntry {
    spans: Vec<WordDiffSpan>,
    epoch: u64,
}

#[derive(Debug, Clone)]
pub struct VirtualDiffRows {
    revision: String,
    rows: Vec<DiffRow>,
    word_cache: BTreeMap<WordCacheKey, WordCacheEntry>,
    epoch: u64,
}

impl VirtualDiffRows {
    pub fn new(revision: impl Into<String>, files: &[UnifiedDiffFile]) -> Self {
        let mut rows = Vec::new();
        for (file_index, file) in files.iter().enumerate() {
            let file_path = file.display_path().to_string();
            for (line_index, line) in file.lines.iter().enumerate() {
                if rows.len() >= DIFF_MAX_ROWS {
                    break;
                }
                rows.push(DiffRow {
                    id: format!("diff:{file_index}:{line_index}"),
                    file_index,
                    file_path: file_path.clone(),
                    kind: line.kind,
                    old_line: line.old_line,
                    new_line: line.new_line,
                    content: line.content.clone(),
                });
            }
        }
        Self {
            revision: revision.into(),
            rows,
            word_cache: BTreeMap::new(),
            epoch: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn visible_window(
        &mut self,
        start: usize,
        length: usize,
        overscan: usize,
    ) -> Vec<PreparedDiffRow> {
        let requested_start = start.min(self.rows.len());
        let requested_end = requested_start.saturating_add(length).min(self.rows.len());
        let warm_start = requested_start.saturating_sub(overscan);
        let warm_end = requested_end.saturating_add(overscan).min(self.rows.len());
        for index in warm_start..warm_end {
            if matches!(
                self.rows[index].kind,
                UnifiedDiffLineKind::Add | UnifiedDiffLineKind::Delete
            ) {
                self.word_spans(index);
            }
        }

        let mut prepared = Vec::with_capacity(requested_end.saturating_sub(requested_start));
        for index in requested_start..requested_end {
            let row = self.rows[index].clone();
            let word_spans = if matches!(
                row.kind,
                UnifiedDiffLineKind::Add | UnifiedDiffLineKind::Delete
            ) {
                self.word_spans(index)
            } else {
                Vec::new()
            };
            prepared.push(PreparedDiffRow { row, word_spans });
        }
        prepared
    }

    pub fn row(&self, index: usize) -> Option<&DiffRow> {
        self.rows.get(index)
    }

    /// Prepare one row for a virtual consumer without materializing a larger
    /// window. Commit previews use this when collapsed files make the visible
    /// rows non-contiguous in the underlying patch.
    pub fn prepared_row(&mut self, index: usize) -> Option<PreparedDiffRow> {
        let row = self.rows.get(index)?.clone();
        let word_spans = if matches!(
            row.kind,
            UnifiedDiffLineKind::Add | UnifiedDiffLineKind::Delete
        ) {
            self.word_spans(index)
        } else {
            Vec::new()
        };
        Some(PreparedDiffRow { row, word_spans })
    }

    fn word_spans(&mut self, index: usize) -> Vec<WordDiffSpan> {
        let row = &self.rows[index];
        let key = WordCacheKey {
            revision: self.revision.clone(),
            row_id: row.id.clone(),
        };
        self.epoch = self.epoch.saturating_add(1).max(1);
        if let Some(entry) = self.word_cache.get_mut(&key) {
            entry.epoch = self.epoch;
            return entry.spans.clone();
        }
        let counterpart = adjacent_counterpart(&self.rows, index);
        let spans = counterpart
            .map(|other| word_diff(&row.content, &other.content, row.kind))
            .unwrap_or_else(|| {
                vec![WordDiffSpan {
                    kind: if row.kind == UnifiedDiffLineKind::Add {
                        WordDiffKind::Added
                    } else {
                        WordDiffKind::Deleted
                    },
                    text: row.content.clone(),
                }]
            });
        self.word_cache.insert(
            key,
            WordCacheEntry {
                spans: spans.clone(),
                epoch: self.epoch,
            },
        );
        while self.word_cache.len() > DIFF_WORD_CACHE_ROWS {
            let Some(oldest) = self
                .word_cache
                .iter()
                .min_by_key(|(_, entry)| entry.epoch)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.word_cache.remove(&oldest);
        }
        spans
    }

    pub fn cached_word_rows(&self) -> usize {
        self.word_cache.len()
    }

    pub fn estimated_bytes(&self) -> usize {
        self.revision.len().saturating_add(
            self.rows
                .iter()
                .map(|row| {
                    row.id
                        .len()
                        .saturating_add(row.file_path.len())
                        .saturating_add(row.content.len())
                })
                .sum::<usize>(),
        )
    }
}

fn adjacent_counterpart(rows: &[DiffRow], index: usize) -> Option<&DiffRow> {
    let row = rows.get(index)?;
    let expected = if row.kind == UnifiedDiffLineKind::Add {
        UnifiedDiffLineKind::Delete
    } else {
        UnifiedDiffLineKind::Add
    };
    index
        .checked_sub(1)
        .and_then(|index| rows.get(index))
        .filter(|candidate| candidate.file_index == row.file_index && candidate.kind == expected)
        .or_else(|| {
            rows.get(index + 1).filter(|candidate| {
                candidate.file_index == row.file_index && candidate.kind == expected
            })
        })
}

fn word_diff(left: &str, right: &str, row_kind: UnifiedDiffLineKind) -> Vec<WordDiffSpan> {
    let left_words = split_words(left);
    let right_words = split_words(right);
    let (current, other, changed_kind) = if row_kind == UnifiedDiffLineKind::Add {
        (&left_words, &right_words, WordDiffKind::Added)
    } else {
        (&left_words, &right_words, WordDiffKind::Deleted)
    };
    let prefix = current
        .iter()
        .zip(other.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = current[prefix..]
        .iter()
        .rev()
        .zip(other[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count()
        .min(current.len().saturating_sub(prefix));
    let mut spans = Vec::new();
    push_words(&mut spans, WordDiffKind::Equal, &current[..prefix]);
    push_words(
        &mut spans,
        changed_kind,
        &current[prefix..current.len().saturating_sub(suffix)],
    );
    if suffix > 0 {
        push_words(
            &mut spans,
            WordDiffKind::Equal,
            &current[current.len() - suffix..],
        );
    }
    spans
}

fn split_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut whitespace = None;
    for character in value.chars() {
        let is_whitespace = character.is_whitespace();
        if whitespace.is_some_and(|previous| previous != is_whitespace) {
            words.push(std::mem::take(&mut current));
        }
        whitespace = Some(is_whitespace);
        current.push(character);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn push_words(spans: &mut Vec<WordDiffSpan>, kind: WordDiffKind, words: &[String]) {
    if words.is_empty() {
        return;
    }
    spans.push(WordDiffSpan {
        kind,
        text: words.concat(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_handles_rename_binary_crlf_and_no_newline_marker() {
        let diff = concat!(
            "diff --git \"a/old name.txt\" \"b/new name.txt\"\r\n",
            "similarity index 100%\r\n",
            "rename from old name.txt\r\n",
            "rename to new name.txt\r\n",
            "--- \"a/old name.txt\"\r\n",
            "+++ \"b/new name.txt\"\r\n",
            "@@ -1 +1 @@\r\n",
            "-old\r\n",
            "+new\r\n",
            "\\ No newline at end of file\r\n",
            "diff --git a/a.png b/a.png\n",
            "Binary files a/a.png and b/a.png differ\n",
        );
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].old_path.as_deref(), Some("old name.txt"));
        assert_eq!(files[0].new_path.as_deref(), Some("new name.txt"));
        assert!(files[0].renamed);
        assert_eq!(files[0].lines.len(), 4);
        assert!(files[1].binary);
    }

    #[test]
    fn parser_decodes_git_octal_quoted_utf8_paths() {
        let files =
            parse_unified_diff("diff --git \"a/\\344\\270\\255.txt\" \"b/\\344\\270\\255.txt\"\n");
        assert_eq!(files[0].display_path(), "中.txt");
    }

    #[test]
    fn hunk_counts_zero_and_explicit_ranges() {
        let files = parse_unified_diff(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -0,0 +1,2 @@\n+one\n+two\n",
        );
        assert_eq!(files[0].lines[1].new_line, Some(1));
        assert_eq!(files[0].lines[2].new_line, Some(2));
    }

    #[test]
    fn trailing_terminator_and_malformed_hunk_do_not_create_phantom_rows() {
        let files =
            parse_unified_diff("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n");
        assert_eq!(files[0].lines.len(), 3);
        assert_eq!(files[0].lines[0].kind, UnifiedDiffLineKind::Hunk);

        let malformed = parse_unified_diff(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ malformed @@\n-old\n+new\n",
        );
        assert!(malformed[0].lines.is_empty());
        assert!(
            malformed[0]
                .header
                .iter()
                .any(|line| line == "@@ malformed @@")
        );
    }

    #[test]
    fn word_diff_is_prepared_only_for_visible_rows_and_cache_is_bounded() {
        let files = parse_unified_diff(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old value\n+new value\n",
        );
        let mut rows = VirtualDiffRows::new("r1", &files);
        assert_eq!(rows.cached_word_rows(), 0);
        let visible = rows.visible_window(1, 1, 0);
        assert_eq!(visible.len(), 1);
        assert_eq!(rows.cached_word_rows(), 1);
        assert!(
            visible[0]
                .word_spans
                .iter()
                .any(|span| span.kind == WordDiffKind::Deleted)
        );
    }

    #[test]
    fn deep_window_returns_the_exact_requested_range_while_warming_overscan() {
        let file = UnifiedDiffFile {
            old_path: Some("large.rs".into()),
            new_path: Some("large.rs".into()),
            header: Vec::new(),
            lines: (0..20_000)
                .map(|index| UnifiedDiffLine {
                    kind: if index % 2 == 0 {
                        UnifiedDiffLineKind::Delete
                    } else {
                        UnifiedDiffLineKind::Add
                    },
                    old_line: (index % 2 == 0).then_some(index as u32 + 1),
                    new_line: (index % 2 == 1).then_some(index as u32 + 1),
                    content: format!("line {index}"),
                })
                .collect(),
            binary: false,
            renamed: false,
            copied: false,
        };
        let mut rows = VirtualDiffRows::new("large-r1", &[file]);
        let window = rows.visible_window(19_500, 120, DIFF_DEFAULT_OVERSCAN);
        assert_eq!(window.len(), 120);
        assert_eq!(window.first().unwrap().row.id, "diff:0:19500");
        assert_eq!(window.last().unwrap().row.id, "diff:0:19619");
        assert_eq!(rows.row(19_500).unwrap().id, "diff:0:19500");
        assert!(rows.cached_word_rows() >= 120);
        assert!(rows.cached_word_rows() <= 120 + DIFF_DEFAULT_OVERSCAN * 2);
    }
}

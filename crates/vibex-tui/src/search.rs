//! Transcript search: a regex with smart case.
//!
//! The query is always a regular expression, and case sensitivity is derived
//! from the query rather than configured: an all-lowercase query matches
//! either case, and a query with an uppercase letter matches exactly. That is
//! the behaviour a reader expects from a search box, and it means `Foo` and
//! `foo` are both one keystroke away without a separate toggle.
//!
//! An invalid pattern is not an error state the interface hides. The pattern
//! compiles to something that matches nothing, the counter says `bad pattern`,
//! and the user keeps typing — which is what makes a half-written regex
//! usable.
//!
//! Zero-width matches are skipped. A regex like `^` matches at every line
//! start; highlighting nothing at every position would light up the whole
//! transcript and make the counter meaningless.

use std::ops::Range;

use regex::RegexBuilder;

/// A compiled search pattern, plus how it was interpreted.
#[derive(Debug, Clone)]
pub struct SearchPattern {
    regex: regex::Regex,
    /// Whether smart case decided to fold case.
    pub case_insensitive: bool,
}

impl SearchPattern {
    /// Compile `query`, applying smart case.
    pub fn compile(query: &str) -> Result<Self, String> {
        let case_insensitive = !query.chars().any(char::is_uppercase);
        RegexBuilder::new(query)
            .case_insensitive(case_insensitive)
            .build()
            .map(|regex| Self {
                regex,
                case_insensitive,
            })
            .map_err(|error| {
                // The regex crate's message ends with a newline and a caret
                // diagram; the first line names the problem, which is all that
                // fits in a search bar.
                error
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or("invalid pattern")
                    .trim()
                    .to_string()
            })
    }

    /// Byte ranges of every non-empty match in `text`.
    pub fn ranges(&self, text: &str) -> Vec<Range<usize>> {
        self.regex
            .find_iter(text)
            .filter(|found| found.start() < found.end())
            .map(|found| found.start()..found.end())
            .collect()
    }

    /// How many non-empty matches `text` contains.
    pub fn count(&self, text: &str) -> usize {
        self.regex
            .find_iter(text)
            .filter(|found| found.start() < found.end())
            .count()
    }
}

/// One search in progress.
#[derive(Debug, Clone)]
pub struct SearchState {
    pub query: String,
    /// `None` while the pattern does not compile.
    pub pattern: Option<SearchPattern>,
    /// The compile error, when there is one.
    pub error: Option<String>,
    /// Blocks containing at least one match, in conversation order.
    pub matches: Vec<usize>,
    /// Index into `matches`.
    pub current: usize,
    /// Total matches across those blocks, for the counter.
    pub total: usize,
    /// Whether keystrokes are editing the query.
    pub composing: bool,
}

impl SearchState {
    /// A fresh search with an empty query.
    pub fn new() -> Self {
        Self {
            query: String::new(),
            pattern: None,
            error: None,
            matches: Vec::new(),
            current: 0,
            total: 0,
            composing: true,
        }
    }

    /// Replace the query and recompile.
    pub fn set_query(&mut self, query: String) {
        self.composing = true;
        self.query = query;
        match SearchPattern::compile(&self.query) {
            Ok(pattern) => {
                self.pattern = Some(pattern);
                self.error = None;
            }
            Err(error) => {
                self.pattern = None;
                self.error = Some(error);
                self.matches.clear();
                self.total = 0;
                self.current = 0;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// The `i/n` counter's current position, one-based.
    pub fn position(&self) -> usize {
        if self.matches.is_empty() {
            0
        } else {
            self.current.min(self.matches.len() - 1) + 1
        }
    }

    /// Step to the next or previous matching block, wrapping.
    pub fn step(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as isize;
        self.current = (self.current as isize + delta).rem_euclid(len) as usize;
    }

    /// The block the search currently points at.
    pub fn current_block(&self) -> Option<usize> {
        self.matches.get(self.current).copied()
    }

    /// Install fresh matches, keeping the cursor on the same block when it is
    /// still a match.
    pub fn set_matches(&mut self, matches: Vec<usize>, total: usize) {
        let anchor = self.current_block();
        self.matches = matches;
        self.total = total;
        self.current = anchor
            .and_then(|block| {
                self.matches
                    .iter()
                    .position(|candidate| *candidate == block)
            })
            .unwrap_or(0);
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lowercase_query_ignores_case_and_an_uppercase_one_does_not() {
        let lower = SearchPattern::compile("upload").expect("compiles");
        assert!(lower.case_insensitive);
        assert_eq!(lower.count("Upload and UPLOAD and upload"), 3);

        let upper = SearchPattern::compile("Upload").expect("compiles");
        assert!(!upper.case_insensitive);
        assert_eq!(upper.count("Upload and UPLOAD and upload"), 1);
    }

    #[test]
    fn the_query_is_a_regular_expression() {
        let pattern = SearchPattern::compile(r"err(or)?\d+").expect("compiles");
        assert_eq!(pattern.count("error42 err7 err"), 2);
    }

    #[test]
    fn an_invalid_pattern_reports_an_error_and_matches_nothing() {
        let error = SearchPattern::compile("(unclosed").expect_err("does not compile");
        assert!(!error.is_empty());
        let mut state = SearchState::new();
        state.set_query("(unclosed".to_string());
        assert!(state.pattern.is_none());
        assert!(state.error.is_some());
        assert_eq!(state.total, 0);
    }

    #[test]
    fn zero_width_matches_are_skipped() {
        let pattern = SearchPattern::compile("x*").expect("compiles");
        assert!(pattern.ranges("").is_empty());
        // `x*` also matches the empty string at every other position; only the
        // real `x` counts.
        assert_eq!(pattern.count("axa"), 1);
        let runs = SearchPattern::compile("a*").expect("compiles");
        assert_eq!(runs.count("aXa"), 2);
    }

    #[test]
    fn stepping_wraps_in_both_directions() {
        let mut state = SearchState::new();
        state.set_query("a".to_string());
        state.set_matches(vec![1, 4, 9], 3);
        assert_eq!(state.current_block(), Some(1));
        state.step(-1);
        assert_eq!(state.current_block(), Some(9));
        state.step(1);
        assert_eq!(state.current_block(), Some(1));
        assert_eq!(state.position(), 1);
    }

    #[test]
    fn refreshing_matches_keeps_the_cursor_on_the_same_block() {
        let mut state = SearchState::new();
        state.set_query("a".to_string());
        state.set_matches(vec![2, 5, 8], 3);
        state.step(1);
        assert_eq!(state.current_block(), Some(5));
        // A streaming append inserts an earlier match; the cursor must not jump.
        state.set_matches(vec![1, 2, 5, 8], 4);
        assert_eq!(state.current_block(), Some(5));
        // A block that no longer matches falls back to the first match.
        state.set_matches(vec![1, 2], 2);
        assert_eq!(state.current_block(), Some(1));
    }
}

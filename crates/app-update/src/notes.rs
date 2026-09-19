//! Localized release-notes selection.
//!
//! Each signed release may publish a plain Markdown notes document beside its
//! signed manifest. The document carries one level-two heading per language, so
//! one published asset serves every locale the desktop can render.
//!
//! Notes are informational: they travel with the verified release tag but they
//! are not covered by the manifest signature, and they never influence update
//! decisions.

/// Largest accepted release-notes document.
pub const MAX_RELEASE_NOTES_BYTES: usize = 256 * 1024;

/// Select the Markdown body that matches `locale`.
///
/// `locale` is a language tag such as `en`, `zh-CN`, or `zh-TW`. Missing
/// translations fall back to the generic Chinese section and then to English,
/// so an older document that only ships `## English` and `## 中文` still
/// resolves for every locale.
pub fn select_notes_section<'a>(document: &'a str, locale: &str) -> Option<&'a str> {
    let document = document.strip_prefix('\u{feff}').unwrap_or(document);
    let sections = document_sections(document);
    for heading in section_candidates(locale) {
        if let Some((_, content)) = sections
            .iter()
            .find(|(name, content)| name.eq_ignore_ascii_case(heading) && !content.is_empty())
        {
            return Some(*content);
        }
    }
    None
}

/// Language sections to try for `locale`, most specific first.
fn section_candidates(locale: &str) -> &'static [&'static str] {
    let locale = locale.trim().to_ascii_lowercase();
    if locale.starts_with("zh") {
        if locale.contains("tw") || locale.contains("hk") || locale.contains("hant") {
            &["繁體中文", "繁体中文", "中文", "English"]
        } else {
            &["简体中文", "簡體中文", "中文", "English"]
        }
    } else {
        &["English"]
    }
}

/// Level-two headings and their bodies, in document order.
fn document_sections(document: &str) -> Vec<(&str, &str)> {
    let mut sections = Vec::new();
    let mut current: Option<(&str, usize)> = None;
    let mut offset = 0usize;
    for line in document.split_inclusive('\n') {
        let text = line.trim_end_matches(['\r', '\n']);
        if let Some(heading) = level_two_heading(text)
            && let Some((name, start)) = current.replace((heading, offset + line.len()))
        {
            sections.push((name, trim_section(&document[start..offset])));
        }
        offset += line.len();
    }
    if let Some((name, start)) = current {
        sections.push((name, trim_section(&document[start..])));
    }
    sections
}

fn level_two_heading(line: &str) -> Option<&str> {
    let heading = line.trim_start().strip_prefix("## ")?;
    let heading = heading.trim();
    (!heading.is_empty()).then_some(heading)
}

/// Drop the horizontal rules a document uses to separate language sections.
fn trim_section(content: &str) -> &str {
    let mut content = content.trim();
    if content.lines().all(is_thematic_break_or_blank) {
        return "";
    }
    while let Some((head, last)) = content.rsplit_once('\n') {
        if !is_thematic_break(last) {
            break;
        }
        content = head.trim_end();
    }
    content
}

fn is_thematic_break(line: &str) -> bool {
    matches!(line.trim(), "---" | "***" | "___")
}

fn is_thematic_break_or_blank(line: &str) -> bool {
    line.trim().is_empty() || is_thematic_break(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCUMENT: &str = "# Vibex v0.2.0 Release Notes\n\n- Released: 2026-08-16\n\n---\n\n## English\n\n### Highlights\n\n- A verified change.\n\n### Fixes\n\n- A verified fix.\n\n---\n\n## 中文\n\n### 亮点\n\n- 一项已验证的变更。\n";

    #[test]
    fn selects_the_section_for_each_supported_locale() {
        let english = select_notes_section(DOCUMENT, "en").unwrap();
        assert!(english.starts_with("### Highlights"));
        assert!(english.contains("A verified fix."));
        assert!(!english.contains("中文"));
        assert!(!english.ends_with("---"));

        let simplified = select_notes_section(DOCUMENT, "zh-CN").unwrap();
        assert!(simplified.starts_with("### 亮点"));
        assert!(!simplified.contains("Highlights"));
    }

    #[test]
    fn language_specific_sections_win_over_the_generic_chinese_section() {
        let document =
            "## English\n\nEN\n\n## 中文\n\nZH\n\n## 繁體中文\n\nTW\n\n## 简体中文\n\nCN\n";
        assert_eq!(select_notes_section(document, "zh-TW"), Some("TW"));
        assert_eq!(select_notes_section(document, "zh-Hant"), Some("TW"));
        assert_eq!(select_notes_section(document, "zh-CN"), Some("CN"));
        assert_eq!(select_notes_section(document, "zh"), Some("CN"));
    }

    #[test]
    fn falls_back_to_chinese_then_english() {
        let english_only = "## English\n\nEN\n";
        assert_eq!(select_notes_section(english_only, "zh-CN"), Some("EN"));

        let generic_chinese = "## English\n\nEN\n\n## 中文\n\nZH\n";
        assert_eq!(select_notes_section(generic_chinese, "zh-TW"), Some("ZH"));

        let unknown = "## English\n\nEN\n";
        assert_eq!(select_notes_section(unknown, "fr"), Some("EN"));
    }

    #[test]
    fn subheadings_do_not_end_a_section() {
        let document =
            "## English\n\n### Highlights\n\n- one\n\n#### Deeper\n\n- two\n\n## 中文\n\nZH\n";
        let english = select_notes_section(document, "en").unwrap();
        assert!(english.contains("- two"));
        assert!(!english.contains("ZH"));
    }

    #[test]
    fn tolerates_crlf_bom_and_missing_sections() {
        let document = "\u{feff}# Notes\r\n\r\n## English\r\n\r\nLine one.\r\n\r\n---\r\n\r\n## 中文\r\n\r\nZH\r\n";
        assert_eq!(select_notes_section(document, "en"), Some("Line one."));

        assert_eq!(
            select_notes_section("# Notes\n\nNo language sections.\n", "en"),
            None
        );
        assert_eq!(
            select_notes_section("## English\n\n## 中文\n\nZH\n", "en"),
            None
        );
        // An empty section must not shadow the next candidate.
        assert_eq!(
            select_notes_section("## 简体中文\n\n## 中文\n\nZH\n", "zh-CN"),
            Some("ZH")
        );
        assert_eq!(select_notes_section("", "en"), None);
    }
}

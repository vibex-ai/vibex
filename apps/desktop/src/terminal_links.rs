//! Finds the links a terminal row paints, so a plain `https://…` in a program's
//! output is clickable like an `OSC 8` hyperlink is.
//!
//! Programs that emit `OSC 8` already hand the terminal a URL per cell. Most do
//! not: an Agent prints a pull-request URL as ordinary text, and before this a
//! reader had to select it and copy it out. The scan is deliberately narrow —
//! only `http`/`https` and bare `www.` runs, with prose punctuation trimmed —
//! because the only action a detected link can take is opening the browser.
//!
//! Columns, not bytes, are what the paint pass can use: a row of CJK text makes
//! one character cover two cells, so every byte offset is mapped back to the
//! cell it came from while the text is built.

use std::ops::Range;

/// A URL found in one painted row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowLink {
    /// Half-open column range within the row.
    pub columns: Range<u16>,
    /// The URL to open. A bare `www.` match carries the scheme it was missing.
    pub url: String,
}

/// Finds every link in one row.
///
/// `cells` are `(column, text)` pairs in paint order. Wide-character spacers and
/// empty cells are the caller's to skip; an empty `text` contributes no byte.
pub fn find_links(cells: &[(u16, String)]) -> Vec<RowLink> {
    let mut text = String::new();
    // One entry per byte of `text`: the cell the byte was painted in.
    let mut byte_to_column = Vec::new();
    for (column, cell) in cells {
        if cell.is_empty() {
            continue;
        }
        for _ in 0..cell.len() {
            byte_to_column.push(*column);
        }
        text.push_str(cell);
    }
    if text.is_empty() || byte_to_column.len() != text.len() {
        return Vec::new();
    }

    let bytes = text.as_bytes();
    let mut links = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let Some(scheme) = scheme_at(bytes, index) else {
            index += 1;
            continue;
        };
        if !is_boundary_before(bytes, index) {
            index += 1;
            continue;
        }
        let content_start = index + scheme.length;
        let mut end = scan_end(bytes, content_start);
        while end > content_start && is_trailing_punctuation(bytes[end - 1]) {
            end -= 1;
        }
        // A bare `www.` still needs a host after the dot, and every scheme needs
        // at least one character of path or host.
        if end <= content_start {
            index = content_start.max(index + 1);
            continue;
        }
        let raw = &text[index..end];
        let url = if scheme.needs_scheme {
            format!("https://{raw}")
        } else {
            raw.to_string()
        };
        // Every cell contributes whole characters, so a match index is always
        // on a UTF-8 boundary; the lookup is still guarded because the column
        // table is the only authority on where a byte was painted.
        let (Some(start_column), Some(end_column)) =
            (byte_to_column.get(index), byte_to_column.get(end - 1))
        else {
            index = end.max(index + 1);
            continue;
        };
        links.push(RowLink {
            columns: *start_column..end_column.saturating_add(1),
            url,
        });
        index = end.max(index + 1);
    }
    links
}

/// The link covering `column`, if any.
pub fn link_at(links: &[RowLink], column: u16) -> Option<&RowLink> {
    links.iter().find(|link| link.columns.contains(&column))
}

struct Scheme {
    length: usize,
    /// `www.example.com` has no scheme; the opener supplies `https://`.
    needs_scheme: bool,
}

fn scheme_at(bytes: &[u8], index: usize) -> Option<Scheme> {
    const SCHEMES: [&[u8]; 2] = [b"https://", b"http://"];
    for scheme in SCHEMES {
        if bytes[index..].starts_with(scheme) {
            return Some(Scheme {
                length: scheme.len(),
                needs_scheme: false,
            });
        }
    }
    if bytes[index..].starts_with(b"www.") {
        return Some(Scheme {
            length: 4,
            needs_scheme: true,
        });
    }
    None
}

/// Whether a link may start at `index`: the row edge, or after punctuation a
/// writer would put before a URL. `xhttps://` inside an identifier must not
/// become a link.
fn is_boundary_before(bytes: &[u8], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let previous = bytes[index - 1];
    previous.is_ascii_whitespace()
        || matches!(
            previous,
            b'(' | b'[' | b'{' | b'<' | b'"' | b'\'' | b'`' | b'|' | b':' | b'=' | b','
        )
        || previous >= 0x80
}

/// The byte at which the URL stops. Whitespace and the characters a shell or
/// Markdown uses as delimiters end it; everything else is part of the URL.
fn scan_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() {
        let byte = bytes[end];
        if byte.is_ascii_whitespace()
            || matches!(
                byte,
                b'<' | b'>' | b'"' | b'`' | b'\'' | b'|' | b'\\' | b'^'
            )
        {
            break;
        }
        end += 1;
    }
    end
}

/// Prose punctuation that follows a URL rather than belonging to it.
fn is_trailing_punctuation(byte: u8) -> bool {
    matches!(
        byte,
        b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b']' | b'}' | b'\'' | b'"'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Vec<(u16, String)> {
        text.chars()
            .enumerate()
            .map(|(column, character)| (column as u16, character.to_string()))
            .collect()
    }

    #[test]
    fn finds_a_url_between_words() {
        let links = find_links(&row("open https://example.com/docs now"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.com/docs");
        assert_eq!(links[0].columns, 5..29);
    }

    #[test]
    fn trims_prose_punctuation_and_closing_brackets() {
        let links = find_links(&row("(see https://example.com/a)."));
        assert_eq!(links[0].url, "https://example.com/a");
        assert_eq!(links[0].columns, 5..26);

        let links = find_links(&row("https://example.com/x,"));
        assert_eq!(links[0].url, "https://example.com/x");
    }

    #[test]
    fn a_bare_www_host_gains_its_scheme() {
        let links = find_links(&row("visit www.example.com today"));
        assert_eq!(links[0].url, "https://www.example.com");
        assert_eq!(links[0].columns, 6..21);
    }

    #[test]
    fn does_not_start_inside_an_identifier() {
        assert!(find_links(&row("xhttps://example.com")).is_empty());
        assert!(find_links(&row("foohttps://example.com")).is_empty());
        // A shell assignment is a real boundary, and the URL is still a link.
        assert_eq!(find_links(&row("url=https://example.com")).len(), 1);
    }

    #[test]
    fn maps_matches_back_to_cells_with_wide_characters() {
        // 中文 occupies two cells each; the URL starts at cell 4.
        let mut cells = vec![
            (0, "中".to_string()),
            (2, "文".to_string()),
            (4, " ".to_string()),
        ];
        // One cell per character, the way a terminal grid actually stores it.
        for (offset, character) in "https://example.com".chars().enumerate() {
            cells.push((5 + offset as u16, character.to_string()));
        }
        let links = find_links(&cells);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].columns, 5..24);
    }

    #[test]
    fn only_http_links_are_detected() {
        assert!(find_links(&row("file:///etc/passwd")).is_empty());
        assert!(find_links(&row("mailto:someone@example.com")).is_empty());
    }

    #[test]
    fn link_at_uses_the_half_open_column_range() {
        let links = find_links(&row("go https://example.com"));
        assert!(link_at(&links, 2).is_none());
        assert!(link_at(&links, 3).is_some());
        assert!(link_at(&links, 21).is_some(), "the last cell is included");
        assert!(link_at(&links, 22).is_none(), "one past the end is outside");
    }
}

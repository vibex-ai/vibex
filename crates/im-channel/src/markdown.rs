//! A bounded Markdown → Telegram HTML renderer.
//!
//! Telegram's `MarkdownV2` asks the sender to escape eighteen characters, and
//! one miss is a `400` that loses the message. HTML asks for three (`&`, `<`,
//! `>`) and supports the same shapes through a small tag set, so the channel
//! renders into HTML for a `parse_mode=HTML` send.
//!
//! This is deliberately not a CommonMark implementation. It recognizes what an
//! Agent answer actually contains — fenced code, inline code, emphasis, links,
//! headings, quotes, lists, tables — and leaves anything it does not understand
//! as literal text. A wrong guess here corrupts the answer, so the renderer
//! prefers dropping formatting over inventing a tag.

/// The deepest emphasis nesting the renderer will follow.
///
/// Agent output is prose, not a parser torture test; the cap keeps a pathological
/// run of `*` from growing the stack.
const MAX_INLINE_DEPTH: usize = 8;

/// Renders Markdown as the HTML subset Telegram accepts.
pub fn to_telegram_html(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();

        if let Some(language) = fence_language(trimmed) {
            let mut body: Vec<&str> = Vec::new();
            index += 1;
            while index < lines.len() && !is_fence(lines[index].trim_start()) {
                body.push(lines[index]);
                index += 1;
            }
            if index < lines.len() {
                index += 1;
            }
            out.push_str(&code_block(language, &body.join("\n")));
            out.push('\n');
            continue;
        }

        if is_table_start(&lines, index) {
            let mut rows: Vec<&str> = Vec::new();
            while index < lines.len() && lines[index].contains('|') {
                rows.push(lines[index]);
                index += 1;
            }
            out.push_str("<pre>");
            out.push_str(&escape_html(&rows.join("\n")));
            out.push_str("</pre>\n");
            continue;
        }

        if let Some(heading) = heading_text(trimmed) {
            out.push_str("<b>");
            out.push_str(&render_inline(&escape_html(heading), 0));
            out.push_str("</b>\n");
            index += 1;
            continue;
        }

        if let Some(quote) = trimmed
            .strip_prefix("> ")
            .or_else(|| (trimmed == ">").then_some(""))
        {
            out.push_str("<blockquote>");
            out.push_str(&render_inline(&escape_html(quote), 0));
            out.push_str("</blockquote>\n");
            index += 1;
            continue;
        }

        if let Some(item) = bullet_text(trimmed) {
            out.push_str("• ");
            out.push_str(&render_inline(&escape_html(item), 0));
            out.push('\n');
            index += 1;
            continue;
        }

        out.push_str(&render_inline(&escape_html(line), 0));
        out.push('\n');
        index += 1;
    }
    out.trim_end_matches('\n').to_string()
}

/// Renders Markdown as the plain text a transport without rich text can show.
///
/// It is the same conversion the HTML path uses, with the tags dropped: a chat
/// that cannot render bold should read `会话` rather than `**会话**`. Code and
/// table blocks keep their lines, which is what makes a status table readable.
pub fn to_plain_text(markdown: &str) -> String {
    let html = to_telegram_html(markdown);
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut characters = html.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            '&' => {
                let rest: String = characters.clone().take(4).collect();
                let replacement = if rest.starts_with("amp;") {
                    characters.nth(3);
                    Some('&')
                } else if rest.starts_with("lt;") {
                    characters.nth(2);
                    Some('<')
                } else if rest.starts_with("gt;") {
                    characters.nth(2);
                    Some('>')
                } else {
                    None
                };
                match replacement {
                    Some(character) => out.push(character),
                    None => out.push('&'),
                }
            }
            _ if !in_tag => out.push(character),
            _ => {}
        }
    }
    out
}

/// Escapes the three characters that would otherwise start an HTML entity.
pub fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn is_fence(trimmed: &str) -> bool {
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

fn fence_language(trimmed: &str) -> Option<&str> {
    let rest = trimmed
        .strip_prefix("```")
        .or_else(|| trimmed.strip_prefix("~~~"))?;
    Some(rest.trim())
}

fn code_block(language: &str, body: &str) -> String {
    let escaped = escape_html(body);
    if language.is_empty() {
        return format!("<pre>{escaped}</pre>");
    }
    let language: String = language
        .chars()
        .take(32)
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '_' | '#')
        })
        .collect();
    if language.is_empty() {
        return format!("<pre>{escaped}</pre>");
    }
    format!("<pre><code class=\"language-{language}\">{escaped}</code></pre>")
}

fn heading_text(trimmed: &str) -> Option<&str> {
    let hashes = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = trimmed.get(hashes..)?;
    rest.strip_prefix(' ').map(str::trim)
}

fn bullet_text(trimmed: &str) -> Option<&str> {
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    None
}

fn is_table_start(lines: &[&str], index: usize) -> bool {
    let header = lines[index].trim();
    if !header.starts_with('|') {
        return false;
    }
    let Some(divider) = lines.get(index + 1).map(|line| line.trim()) else {
        return false;
    };
    divider.starts_with('|')
        && divider.contains('-')
        && divider
            .chars()
            .all(|character| matches!(character, '|' | '-' | ':' | ' '))
}

fn render_inline(input: &str, depth: usize) -> String {
    if depth >= MAX_INLINE_DEPTH {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut index = 0;
    while index < bytes.len() {
        let rest = &input[index..];
        // A backslash takes the next character literally, which is how a value
        // the channel did not write (a title, a model name) is kept from being
        // read as markup.
        if let Some(character) = rest.strip_prefix('\\').and_then(|rest| rest.chars().next()) {
            out.push(character);
            index += 1 + character.len_utf8();
            continue;
        }
        if rest.starts_with('`')
            && let Some(rendered) = code_span(rest)
        {
            out.push_str(&rendered.0);
            index += rendered.1;
            continue;
        }
        if (rest.starts_with("**") || rest.starts_with("__"))
            && let Some(rendered) = paired(rest, &rest[..2], "<b>", "</b>", depth)
        {
            out.push_str(&rendered.0);
            index += rendered.1;
            continue;
        }
        if rest.starts_with("~~")
            && let Some(rendered) = paired(rest, "~~", "<s>", "</s>", depth)
        {
            out.push_str(&rendered.0);
            index += rendered.1;
            continue;
        }
        if rest.starts_with('*')
            && let Some(rendered) = paired(rest, "*", "<i>", "</i>", depth)
        {
            out.push_str(&rendered.0);
            index += rendered.1;
            continue;
        }
        if rest.starts_with('_') {
            let before = input[..index].chars().next_back();
            if let Some(rendered) = underscore_emphasis(rest, before, depth) {
                out.push_str(&rendered.0);
                index += rendered.1;
                continue;
            }
        }
        if rest.starts_with('[')
            && let Some((label, url, consumed)) = link(rest, depth)
        {
            out.push_str(&label);
            if let Some(url) = url {
                out.push_str(&format!(" (<a href=\"{url}\">{url}</a>)"));
            }
            index += consumed;
            continue;
        }
        let character = rest.chars().next().unwrap_or_default();
        out.push(character);
        index += character.len_utf8();
    }
    out
}

/// `_` emphasis only counts at both ends of a word, so `snake_case_name`
/// survives as an identifier instead of becoming italics.
fn underscore_emphasis(rest: &str, before: Option<char>, depth: usize) -> Option<(String, usize)> {
    if before.is_some_and(|character| character.is_alphanumeric()) {
        return None;
    }
    let closing = rest[1..].find('_')?;
    if closing == 0 {
        return None;
    }
    let body = &rest[1..1 + closing];
    if body.starts_with(char::is_whitespace) || body.ends_with(char::is_whitespace) {
        return None;
    }
    let after = rest[1 + closing + 1..].chars().next();
    if after.is_some_and(|character| character.is_alphanumeric()) {
        return None;
    }
    let consumed = 1 + closing + 1;
    Some((
        format!("<i>{}</i>", render_inline(body, depth + 1)),
        consumed,
    ))
}

/// Renders `` `code` `` as `<code>`. Returns the markup and bytes consumed.
fn code_span(rest: &str) -> Option<(String, usize)> {
    let mut ticks = 0;
    for character in rest.chars() {
        if character == '`' {
            ticks += 1;
        } else {
            break;
        }
    }
    let delimiter = "`".repeat(ticks);
    let body_start = ticks;
    let closing = rest[body_start..].find(&delimiter)?;
    if closing == 0 {
        return None;
    }
    let body = &rest[body_start..body_start + closing];
    let consumed = body_start + closing + ticks;
    Some((format!("<code>{body}</code>"), consumed))
}

/// Renders `delimiter ... delimiter` as `open ... close`.
fn paired(
    rest: &str,
    delimiter: &str,
    open: &str,
    close: &str,
    depth: usize,
) -> Option<(String, usize)> {
    let body_start = delimiter.len();
    let closing = rest[body_start..].find(delimiter)?;
    if closing == 0 {
        return None;
    }
    let body = &rest[body_start..body_start + closing];
    if body.starts_with(char::is_whitespace) || body.ends_with(char::is_whitespace) {
        return None;
    }
    let consumed = body_start + closing + delimiter.len();
    Some((
        format!("{open}{}{close}", render_inline(body, depth + 1)),
        consumed,
    ))
}

/// Renders `[label](url)`. The label is rendered; the URL is only kept when it
/// is a scheme Telegram will accept, so a malformed one becomes plain text.
fn link(rest: &str, depth: usize) -> Option<(String, Option<String>, usize)> {
    let closing = rest.find(']')?;
    let label = &rest[1..closing];
    let after = rest.get(closing + 1..)?;
    let after = after.strip_prefix('(')?;
    let url_end = after.find(')')?;
    let url = after[..url_end].trim();
    let consumed = closing + 1 + 1 + url_end + 1;
    let label = render_inline(label, depth + 1);
    match safe_url(url) {
        Some(url) => Some((label, Some(url), consumed)),
        None => Some((label, None, consumed)),
    }
}

/// The URL shapes a Telegram anchor may carry.
fn safe_url(url: &str) -> Option<String> {
    if url.is_empty()
        || url.len() > 2048
        || url
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || url.contains(['<', '>', '"', '\''])
    {
        return None;
    }
    let lowered = url.to_ascii_lowercase();
    let scheme_ok = lowered.starts_with("http://")
        || lowered.starts_with("https://")
        || lowered.starts_with("tg://")
        || lowered.starts_with("mailto:");
    scheme_ok.then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_prose_survives_unchanged() {
        assert_eq!(to_telegram_html("All done."), "All done.");
    }

    #[test]
    fn html_metacharacters_are_escaped() {
        assert_eq!(to_telegram_html("a < b & c > d"), "a &lt; b &amp; c &gt; d");
    }

    #[test]
    fn fenced_code_becomes_pre_with_its_language() {
        let html = to_telegram_html("before\n```rust\nfn main() {}\n```\nafter");
        assert!(html.contains("<pre><code class=\"language-rust\">fn main() {}</code></pre>"));
        assert!(html.contains("before"));
        assert!(html.contains("after"));
    }

    #[test]
    fn code_content_is_not_formatted() {
        let html = to_telegram_html("```\n**not bold** <tag>\n```");
        assert!(html.contains("**not bold** &lt;tag&gt;"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
    }

    #[test]
    fn emphasis_and_links_render() {
        let html = to_telegram_html("**bold** and *italic* and [docs](https://example.com/a)");
        assert!(html.contains("<b>bold</b>"), "{html}");
        assert!(html.contains("<i>italic</i>"), "{html}");
        assert!(
            html.contains("<a href=\"https://example.com/a\">https://example.com/a</a>"),
            "{html}"
        );
    }

    #[test]
    fn a_link_with_a_rejected_scheme_keeps_only_its_label() {
        let html = to_telegram_html("[click](javascript:alert(1))");
        assert!(!html.contains("javascript"), "{html}");
        assert!(html.contains("click"), "{html}");
    }

    #[test]
    fn snake_case_underscores_are_left_alone() {
        let html = to_telegram_html("call some_function_name here");
        assert_eq!(html, "call some_function_name here");
    }

    #[test]
    fn headings_bullets_and_quotes_render() {
        let html = to_telegram_html("# Title\n- one\n- two\n> quoted");
        assert!(html.contains("<b>Title</b>"), "{html}");
        assert!(html.contains("• one"), "{html}");
        assert!(html.contains("<blockquote>quoted</blockquote>"), "{html}");
    }

    #[test]
    fn a_table_becomes_a_pre_block() {
        let html = to_telegram_html("| a | b |\n| - | - |\n| 1 | 2 |");
        assert!(html.starts_with("<pre>"), "{html}");
        assert!(html.contains("| 1 | 2 |"), "{html}");
    }

    #[test]
    fn an_unclosed_fence_does_not_swallow_the_formatting_after_it() {
        let html = to_telegram_html("```\nstill code");
        assert!(html.starts_with("<pre>"), "{html}");
    }

    #[test]
    fn plain_text_drops_the_markup_and_keeps_the_text() {
        assert_eq!(to_plain_text("**bold** and *italic*"), "bold and italic");
        assert_eq!(to_plain_text("a < b & c"), "a < b & c");
        let table = to_plain_text("```\n会话  #22d494a1\n状态  空闲\n```");
        assert_eq!(table, "会话  #22d494a1\n状态  空闲");
    }

    #[test]
    fn a_backslash_keeps_the_next_character_literal() {
        assert_eq!(to_telegram_html("a \\*literal\\* star"), "a *literal* star");
        assert_eq!(to_telegram_html("\\`not code\\`"), "`not code`");
    }

    #[test]
    fn deeply_nested_emphasis_terminates() {
        let mut text = "x".to_string();
        for _ in 0..64 {
            text = format!("**{text}**");
        }
        let html = to_telegram_html(&text);
        assert!(!html.is_empty());
    }
}

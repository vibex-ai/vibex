//! Find in page.
//!
//! Chrome's own find bar is part of the browser UI, which a pipe-mode embed
//! does not have, so the search runs in the page and the panel draws the bar.
//!
//! Two deliberate choices:
//!
//! - **The page's DOM is not rewritten.** Wrapping every hit in `<mark>` would
//!   be the obvious implementation and it breaks the page: React, Vue and
//!   Svelte all diff their own children, and a foreign node inside their tree
//!   is enough to make the next render throw. The CSS Custom Highlight API
//!   paints the same rectangles from ranges, with nothing inserted.
//! - **The active hit is an element, not a range.** Chrome will not scroll to
//!   an arbitrary range, so the range's element gets a marker attribute and the
//!   scroll goes through the element. The attribute is removed on the next
//!   search and when the bar closes.

use serde_json::{Value, json};

/// Matches the page will paint before it stops counting.
///
/// A short query on a large documentation page can match tens of thousands of
/// times; the reader wants to know "many", not an exact number, and the walk
/// has to stay inside one frame.
const MAX_MATCHES: usize = 2_000;

/// The id of the injected style element.
const STYLE_ID: &str = "vibex-find-style";

/// Builds the script that searches the page.
///
/// `forward` only matters when the query is the same as the previous call: that
/// is a "next"/"previous" step. A different query starts from the first match
/// (or the last, when searching backwards).
pub fn find_script(query: &str, forward: bool) -> String {
    let query = serde_json::to_string(query).unwrap_or_else(|_| "\"\"".to_string());
    let direction = if forward { "true" } else { "false" };
    format!(
        r#"(() => {{
  const query = {query};
  const forward = {direction};
  const MAX = {MAX_MATCHES};
  const STYLE_ID = "{STYLE_ID}";
  const ACTIVE = "data-vibex-find-active";
  const supportsHighlights = !!(window.CSS && CSS.highlights && window.Highlight);
  const clear = () => {{
    if (supportsHighlights) {{
      try {{ CSS.highlights.delete("vibex-find"); }} catch (_) {{}}
    }}
    document.querySelectorAll("[" + ACTIVE + "]").forEach((node) => {{
      node.removeAttribute(ACTIVE);
    }});
  }};
  clear();
  if (!query) {{
    const style = document.getElementById(STYLE_ID);
    if (style) style.remove();
    window.__vibexFind = null;
    return {{ total: 0, current: 0 }};
  }}
  if (!document.getElementById(STYLE_ID)) {{
    const style = document.createElement("style");
    style.id = STYLE_ID;
    style.textContent =
      "::highlight(vibex-find){{background-color:#ffd54f;color:#000}}" +
      "[" + ACTIVE + "]{{outline:2px solid #ff8f00;outline-offset:1px}}";
    (document.head || document.documentElement).appendChild(style);
  }}

  // Text nodes only, and never inside a script or a style: `nodeValue` there is
  // code, not content, and a hit in it would be invisible and unclickable.
  const root = document.body || document.documentElement;
  if (!root) return {{ total: 0, current: 0 }};
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {{
    acceptNode(node) {{
      const value = node.nodeValue;
      if (!value) return NodeFilter.FILTER_REJECT;
      const parent = node.parentElement;
      if (!parent) return NodeFilter.FILTER_REJECT;
      const tag = parent.tagName;
      if (tag === "SCRIPT" || tag === "STYLE" || tag === "NOSCRIPT" || tag === "TEMPLATE") {{
        return NodeFilter.FILTER_REJECT;
      }}
      return NodeFilter.FILTER_ACCEPT;
    }},
  }});
  const needle = query.toLowerCase();
  const ranges = [];
  let node;
  while (ranges.length < MAX && (node = walker.nextNode())) {{
    const haystack = node.nodeValue.toLowerCase();
    let index = haystack.indexOf(needle);
    while (index !== -1 && ranges.length < MAX) {{
      const range = document.createRange();
      range.setStart(node, index);
      range.setEnd(node, index + needle.length);
      ranges.push(range);
      index = haystack.indexOf(needle, index + needle.length);
    }}
  }}
  if (ranges.length === 0) {{
    window.__vibexFind = {{ query: query, index: -1 }};
    return {{ total: 0, current: 0 }};
  }}

  // A repeat of the same query is a step; a new query starts at the near end.
  const previous = window.__vibexFind;
  let index;
  if (previous && previous.query === query && previous.index >= 0) {{
    index = forward ? previous.index + 1 : previous.index - 1;
    if (index >= ranges.length) index = 0;
    if (index < 0) index = ranges.length - 1;
  }} else {{
    index = forward ? 0 : ranges.length - 1;
  }}
  window.__vibexFind = {{ query: query, index: index }};

  if (supportsHighlights) {{
    try {{ CSS.highlights.set("vibex-find", new Highlight(...ranges)); }} catch (_) {{}}
  }}
  const active = ranges[index];
  const element = active.startContainer.parentElement;
  if (element) {{
    element.setAttribute(ACTIVE, "1");
    try {{ element.scrollIntoView({{ block: "center", inline: "nearest" }}); }} catch (_) {{}}
  }}
  return {{ total: ranges.length, current: index + 1 }};
}})()"#
    )
}

/// Builds the script that removes the highlights and the injected style.
pub fn clear_find_script() -> String {
    format!(
        r#"(() => {{
  const ACTIVE = "data-vibex-find-active";
  if (window.CSS && CSS.highlights) {{
    try {{ CSS.highlights.delete("vibex-find"); }} catch (_) {{}}
  }}
  document.querySelectorAll("[" + ACTIVE + "]").forEach((node) => {{
    node.removeAttribute(ACTIVE);
  }});
  const style = document.getElementById("{STYLE_ID}");
  if (style) style.remove();
  window.__vibexFind = null;
  return true;
}})()"#
    )
}

/// Parses the answer of [`find_script`].
///
/// A malformed answer is "no matches": the panel then says so instead of
/// showing a stale count from the previous search.
pub fn parse_find_result(value: &Value) -> (u32, u32) {
    let total = value
        .get("total")
        .and_then(Value::as_u64)
        .and_then(|total| u32::try_from(total).ok())
        .unwrap_or(0);
    let current = value
        .get("current")
        .and_then(Value::as_u64)
        .and_then(|current| u32::try_from(current).ok())
        .unwrap_or(0);
    (total, current)
}

/// The params for a `Runtime.evaluate` that carries a find call.
pub fn evaluate_params(script: String) -> Value {
    json!({ "expression": script, "returnByValue": true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_quotes_the_query_and_keeps_the_direction() {
        let script = find_script("needle", true);
        assert!(script.contains(r#"const query = "needle";"#));
        assert!(script.contains("const forward = true;"));
        assert!(find_script("needle", false).contains("const forward = false;"));
    }

    #[test]
    fn a_query_with_quotes_cannot_break_out_of_the_script() {
        let script = find_script("say \"hi\"", true);
        assert!(script.contains(r#"const query = "say \"hi\"";"#));
    }

    #[test]
    fn the_script_never_rewrites_the_page() {
        let script = find_script("x", true);
        // A framework's own tree must not gain a node from a search.
        assert!(!script.contains("createElement(\"mark\")"));
        assert!(!script.contains("appendChild(range"));
        // The two supported paints: the highlight registry and one attribute.
        assert!(script.contains("CSS.highlights.set"));
        assert!(script.contains("data-vibex-find-active"));
    }

    #[test]
    fn clearing_removes_the_style_it_injected() {
        let script = clear_find_script();
        assert!(script.contains("CSS.highlights.delete"));
        assert!(script.contains(STYLE_ID));
        assert!(script.contains("remove()"));
    }

    #[test]
    fn find_results_parse_and_a_malformed_answer_is_empty() {
        assert_eq!(
            parse_find_result(&json!({ "total": 12, "current": 3 })),
            (12, 3)
        );
        assert_eq!(parse_find_result(&Value::Null), (0, 0));
        assert_eq!(parse_find_result(&json!({ "total": -1 })), (0, 0));
    }
}

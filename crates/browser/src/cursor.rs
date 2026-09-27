//! The page's cursor shape, approximated from the hovered element.
//!
//! A screencast frame carries no cursor, so the panel would show an arrow over
//! a link, a text field and a resize handle alike. The page already knows the
//! answer — `getComputedStyle(el).cursor` is what a real browser uses — so the
//! probe reads it and the panel maps it onto a native cursor.
//!
//! This is an approximation and is treated as one: an unknown value, a cursor
//! keyword the platform cannot express (`zoom-in`, `help`), or a probe that
//! fails all leave the previous cursor in place rather than flickering.

use serde_json::{Value, json};

/// Builds the script that reads the computed cursor at a viewport point.
///
/// Coordinates are CSS pixels relative to the viewport, the same space
/// `Input.dispatchMouseEvent` takes.
pub fn cursor_probe_params(x: f64, y: f64) -> Value {
    let script = format!(
        r#"(() => {{
  // `document.elementFromPoint`, not a global: the bare name is a
  // ReferenceError that the probe would report as "no element anywhere".
  const element = document.elementFromPoint({x}, {y});
  if (!element) return null;
  try {{
    return getComputedStyle(element).cursor || "auto";
  }} catch (_) {{
    return null;
  }}
}})()"#
    );
    json!({ "expression": script, "returnByValue": true })
}

/// The cursor keyword the probe answered, if it answered with one.
pub fn parse_cursor(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|cursor| !cursor.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_carries_the_point_and_reads_the_computed_style() {
        let params = cursor_probe_params(12.5, 40.0);
        let expression = params["expression"].as_str().unwrap();
        assert!(expression.contains("document.elementFromPoint(12.5, 40)"));
        assert!(expression.contains("getComputedStyle(element).cursor"));
        assert_eq!(params["returnByValue"], json!(true));
    }

    #[test]
    fn a_point_with_no_element_is_not_a_cursor() {
        assert_eq!(parse_cursor(&Value::Null), None);
        assert_eq!(parse_cursor(&json!("")), None);
        assert_eq!(parse_cursor(&json!("   ")), None);
        assert_eq!(parse_cursor(&json!("pointer")), Some("pointer".to_string()));
    }
}

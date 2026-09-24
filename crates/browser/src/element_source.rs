//! Element ↔ code mapping.
//!
//! This is the one capability that genuinely requires an embedded browser: a
//! standalone Chrome has no idea where the source lives.
//!
//! The mapping is framework-specific and only exists in development builds:
//!
//! | framework | hook | source information |
//! |---|---|---|
//! | React ≤18 | `__REACT_DEVTOOLS_GLOBAL_HOOK__` | `fiber._debugSource` |
//! | React 19+ | `__REACT_DEVTOOLS_GLOBAL_HOOK__` | `_debugStack`, needs source maps |
//! | Vue 3 | `__vueParentComponent` | `type.__file` (file only, no line) |
//! | Svelte | `__svelte_meta` | `loc.file/line/column` |
//!
//! Source maps solve "generated code → original source"; they do not solve
//! "DOM node → generated code". That step needs the framework hook, so the hook
//! is the primary path and source maps are the refinement.
//!
//! When a project is not one of these frameworks, or is a production build, the
//! mapping is unavailable and the caller must say so explicitly rather than
//! failing silently.

use serde_json::{Value, json};
use vibex_core::BrowserElementSource;

/// The JavaScript probe run inside the page.
///
/// It is written to be defensive: dev hooks are absent in production builds,
/// React versions differ, and a probe must never throw into the page.
pub const ELEMENT_SOURCE_PROBE: &str = r#"(() => {
  const target = document.querySelector('[data-vibex-source-target="1"]');
  if (!target) return { framework: 'unknown', detail: 'no element was selected' };
  const result = { framework: 'unknown', path: null, line: null, column: null, component: null,
                   approximate: false, detail: null };

  // Svelte publishes an exact location on the element itself.
  let node = target;
  while (node) {
    if (node.__svelte_meta && node.__svelte_meta.loc) {
      const loc = node.__svelte_meta.loc;
      result.framework = 'svelte';
      result.path = loc.file || null;
      result.line = loc.line || null;
      result.column = loc.column || null;
      return result;
    }
    node = node.parentElement;
  }

  // Vue 3 exposes the component definition, which carries __file in dev builds.
  node = target;
  while (node) {
    if (node.__vueParentComponent) {
      const definition = node.__vueParentComponent.type || {};
      result.framework = 'vue';
      result.path = definition.__file || null;
      result.component = definition.name || definition.__name || null;
      if (!result.path) {
        result.detail = 'the Vue component has no __file; this is a production build';
      } else {
        result.approximate = true;
        result.detail = 'Vue exposes the component file but not the exact line';
      }
      return result;
    }
    node = node.parentElement;
  }

  // React: walk the fiber tree looking for the host instance that owns this node.
  const hook = window.__REACT_DEVTOOLS_GLOBAL_HOOK__;
  if (!hook || !hook.renderers || !hook.getFiberRoots) return result;
  const roots = [];
  hook.renderers.forEach((_renderer, rendererId) => {
    try { hook.getFiberRoots(rendererId).forEach((root) => roots.push(root)); } catch (_) {}
  });
  const stack = roots.map((root) => root.current).filter(Boolean);
  const seen = new Set();
  while (stack.length) {
    const fiber = stack.pop();
    if (!fiber || seen.has(fiber)) continue;
    seen.add(fiber);
    if (fiber.stateNode === target) {
      result.framework = 'react';
      let owner = fiber;
      while (owner) {
        const source = owner._debugSource;
        if (source && source.fileName) {
          result.path = source.fileName;
          result.line = source.lineNumber || null;
          result.column = source.columnNumber || null;
          break;
        }
        if (owner._debugStack && !result.detail) {
          result.approximate = true;
          result.detail = 'React 19 removed _debugSource; the owner stack needs source maps to ' +
                          'resolve a file and line';
        }
        if (owner.type) {
          result.component = owner.type.displayName || owner.type.name || result.component;
        }
        owner = owner._debugOwner || owner.return;
      }
      return result;
    }
    if (fiber.child) stack.push(fiber.child);
    if (fiber.sibling) stack.push(fiber.sibling);
  }
  result.detail = 'the element is not owned by any React fiber';
  return result;
})()"#;

/// Marks an element as the probe target and returns the resolved source.
///
/// The marker attribute is written on the element the caller resolved from a
/// `ref`; it is removed again so the page is left as it was found.
pub fn marker_setup_script() -> &'static str {
    r#"(() => {
  document.querySelectorAll('[data-vibex-source-target="1"]').forEach((node) => {
    node.removeAttribute('data-vibex-source-target');
  });
  return true;
})()"#
}

/// Builds the runtime call that tags an element by backend node id.
pub fn mark_target_script() -> &'static str {
    r#"(() => {
  document.querySelectorAll('[data-vibex-source-target="1"]').forEach((node) => {
    node.removeAttribute('data-vibex-source-target');
  });
  return true;
})()"#
}

/// Parses a probe result into the public contract.
pub fn parse_probe_result(value: &Value) -> BrowserElementSource {
    let framework = value
        .get("framework")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(str::to_string);
    let line = value
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|line| u32::try_from(line).ok());
    let column = value
        .get("column")
        .and_then(Value::as_u64)
        .and_then(|column| u32::try_from(column).ok());
    let component = value
        .get("component")
        .and_then(Value::as_str)
        .filter(|component| !component.is_empty())
        .map(str::to_string);
    let detail = value
        .get("detail")
        .and_then(Value::as_str)
        .filter(|detail| !detail.is_empty())
        .map(str::to_string);

    let mut approximate = value
        .get("approximate")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut detail = detail;
    if path.is_none() {
        approximate = true;
        detail.get_or_insert_with(|| match framework.as_str() {
            "react" => "no source location was found for this element; element↔code mapping needs \
                        a development build"
                .to_string(),
            "vue" => "the Vue component exposes no file; element↔code mapping needs a development \
                      build"
                .to_string(),
            "svelte" => "the Svelte component exposes no location".to_string(),
            _ => {
                "this page is not a React, Vue or Svelte development build, so elements cannot be \
                  mapped back to source"
                    .to_string()
            }
        });
    }

    BrowserElementSource {
        path: path.unwrap_or_default(),
        line,
        column,
        component,
        framework,
        approximate,
        detail,
    }
}

/// Strips a development server URL prefix so the path can be opened in the
/// workspace.
///
/// Vite and Next serve module URLs like `/src/App.tsx?t=123` or
/// `http://localhost:5173/src/App.tsx`; the editor wants `src/App.tsx`.
pub fn normalize_source_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Reject traversal *before* URL normalization: `http://x/../secret`
    // normalizes to `/secret`, which would look harmless by the time the path
    // is inspected.
    if trimmed.split(['/', '\\']).any(|segment| segment == "..") {
        return None;
    }
    let without_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    let path = if let Ok(parsed) = url::Url::parse(without_query) {
        parsed.path().to_string()
    } else {
        without_query.to_string()
    };
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return None;
    }
    // Reject anything that would escape the workspace root once joined.
    if path.split('/').any(|segment| segment == "..") {
        return None;
    }
    Some(path.to_string())
}

/// Builds the JSON the page probe is invoked with.
pub fn probe_call_params(backend_node_id: i64) -> Value {
    json!({ "backendNodeId": backend_node_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(framework: &str, path: Option<&str>, line: Option<u32>) -> Value {
        json!({
            "framework": framework,
            "path": path,
            "line": line,
            "column": 3,
            "component": "App",
            "approximate": false,
            "detail": null,
        })
    }

    #[test]
    fn a_react_dev_result_maps_directly() {
        let source = parse_probe_result(&probe("react", Some("/src/App.tsx"), Some(42)));
        assert_eq!(source.framework, "react");
        assert_eq!(source.path, "/src/App.tsx");
        assert_eq!(source.line, Some(42));
        assert!(!source.approximate);
        assert!(source.detail.is_none());
    }

    #[test]
    fn a_missing_path_is_reported_as_unavailable_not_as_success() {
        let source = parse_probe_result(&probe("react", None, None));
        assert!(source.path.is_empty());
        assert!(source.approximate);
        assert!(source.detail.unwrap().contains("development build"));
    }

    #[test]
    fn non_framework_pages_explain_why_mapping_is_impossible() {
        let source = parse_probe_result(&probe("unknown", None, None));
        assert_eq!(source.framework, "unknown");
        assert!(
            source
                .detail
                .unwrap()
                .contains("not a React, Vue or Svelte")
        );
    }

    #[test]
    fn react_19_style_results_stay_marked_approximate() {
        let mut value = probe("react", Some("/src/App.tsx"), None);
        value["approximate"] = json!(true);
        value["detail"] = json!("React 19 removed _debugSource");
        let source = parse_probe_result(&value);
        assert!(source.approximate);
        assert!(source.line.is_none());
        assert!(source.detail.unwrap().contains("React 19"));
    }

    #[test]
    fn probe_source_is_defensive_about_missing_hooks() {
        assert!(ELEMENT_SOURCE_PROBE.contains("__REACT_DEVTOOLS_GLOBAL_HOOK__"));
        assert!(ELEMENT_SOURCE_PROBE.contains("__vueParentComponent"));
        assert!(ELEMENT_SOURCE_PROBE.contains("__svelte_meta"));
        assert!(ELEMENT_SOURCE_PROBE.contains("_debugSource"));
        assert!(ELEMENT_SOURCE_PROBE.contains("_debugStack"));
    }

    #[test]
    fn module_urls_normalize_to_workspace_relative_paths() {
        assert_eq!(
            normalize_source_path("http://localhost:5173/src/App.tsx?t=123"),
            Some("src/App.tsx".to_string())
        );
        assert_eq!(
            normalize_source_path("/src/components/Button.vue"),
            Some("src/components/Button.vue".to_string())
        );
        assert_eq!(
            normalize_source_path("src/main.ts#L10"),
            Some("src/main.ts".to_string())
        );
        assert_eq!(normalize_source_path(""), None);
        assert_eq!(normalize_source_path("/"), None);
    }

    #[test]
    fn path_traversal_is_rejected() {
        assert_eq!(normalize_source_path("/../../etc/passwd"), None);
        assert_eq!(normalize_source_path("http://x/../secret"), None);
    }

    #[test]
    fn probe_call_params_carry_the_backend_node_id() {
        assert_eq!(probe_call_params(77)["backendNodeId"], 77);
    }
}

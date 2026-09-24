//! Accessibility-tree observation.
//!
//! The agent's channel reads `Accessibility.getFullAXTree`, not pixels. An AX
//! tree is text, cheap and semantically exact, and it is the same data the
//! browser exposes to assistive technology.
//!
//! The raw tree is far too large for a model context and its shape varies by
//! page, so it is pruned deterministically:
//!
//! * depth-limited (`BROWSER_OBSERVE_AX_DEPTH`, or the extended depth);
//! * interactive roles are kept first, then named static content;
//! * `name` / `value` are truncated;
//! * the result is capped at a caller-supplied element budget, and the
//!   truncation is reported rather than hidden.
//!
//! AX nodes carry no coordinates. Anything that needs to click a node has to
//! resolve its `backendDOMNodeId` through `DOM.getBoxModel` first.

use std::collections::HashMap;

use serde_json::Value;
use vibex_core::{
    BROWSER_OBSERVE_AX_DEPTH, BROWSER_OBSERVE_EXTENDED_AX_DEPTH, BROWSER_OBSERVE_MAX_MAX_ELEMENTS,
    BROWSER_OBSERVE_MIN_MAX_ELEMENTS, BrowserElement,
};

/// Longest element name kept in an observation.
pub const MAX_ELEMENT_NAME_CHARS: usize = 160;
/// Longest value kept in an observation.
pub const MAX_ELEMENT_VALUE_CHARS: usize = 120;

/// Roles that can be acted on. These are kept before static content when the
/// element budget truncates the tree.
pub const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "checkbox",
    "combobox",
    "disclosure triangle",
    "gridcell",
    "link",
    "listbox",
    "menu",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "scrollbar",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textbox",
    "text field",
    "treeitem",
];

/// Roles that carry no information and only add noise.
pub const IGNORED_ROLES: &[&str] = &[
    "none",
    "presentation",
    "generic",
    "ignored",
    "InlineTextBox",
    "LineBreak",
    "list",
    "group",
    "image",
    "StaticText",
];

/// Roles that describe the document container rather than anything in it.
///
/// These are always dropped: they are not actionable, and keeping them wastes
/// the element budget and confuses the model about what it can click.
pub const STRUCTURAL_ROLES: &[&str] = &["RootWebArea", "WebArea", "document"];

/// True when a role is directly actionable.
pub fn is_interactive_role(role: &str) -> bool {
    let normalized = role.trim().to_ascii_lowercase();
    INTERACTIVE_ROLES.contains(&normalized.as_str()) || normalized == "textbox"
}

fn is_noise_role(role: &str) -> bool {
    let normalized = role.trim().to_ascii_lowercase();
    IGNORED_ROLES.contains(&normalized.as_str())
}

fn truncate(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Clamps a caller-supplied element budget into the accepted range.
pub fn clamp_max_elements(requested: Option<u32>) -> usize {
    let requested = requested.unwrap_or(vibex_core::BROWSER_OBSERVE_DEFAULT_MAX_ELEMENTS as u32);
    (requested as usize).clamp(
        BROWSER_OBSERVE_MIN_MAX_ELEMENTS,
        BROWSER_OBSERVE_MAX_MAX_ELEMENTS,
    )
}

/// Resolves the AX depth for a request.
pub fn resolve_depth(extended: bool) -> u16 {
    if extended {
        BROWSER_OBSERVE_EXTENDED_AX_DEPTH
    } else {
        BROWSER_OBSERVE_AX_DEPTH
    }
}

/// One pruned node, before refs are assigned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedElement {
    pub backend_dom_node_id: Option<i64>,
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub disabled: bool,
}

impl PrunedElement {
    pub fn editable(&self) -> bool {
        matches!(
            self.role.as_str(),
            "textbox" | "searchbox" | "combobox" | "spinbutton" | "text field"
        ) || self.value.is_some()
    }
}

/// The outcome of pruning one accessibility tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedObservation {
    pub elements: Vec<PrunedElement>,
    pub truncated: bool,
}

/// Prunes a raw `Accessibility.getFullAXTree` result.
///
/// `ax_nodes` is the `nodes` array from the CDP response. Nodes whose parent
/// chain exceeds `depth` are dropped. Interactive nodes win the element budget;
/// the remainder keeps document order.
pub fn prune_ax_tree(ax_nodes: &[Value], max_elements: usize, depth: u16) -> PrunedObservation {
    let by_id: HashMap<&str, &Value> = ax_nodes
        .iter()
        .filter_map(|node| {
            node.get("nodeId")
                .and_then(Value::as_str)
                .map(|id| (id, node))
        })
        .collect();

    let mut interactive = Vec::new();
    let mut static_content = Vec::new();
    let mut truncated = false;

    for node in ax_nodes {
        // `ignored` nodes exist for internal bookkeeping only.
        if node
            .get("ignored")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let role = node
            .get("role")
            .and_then(|role| role.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if role.is_empty() || is_noise_role(&role) {
            continue;
        }
        if node_depth(node, &by_id) > depth as usize {
            continue;
        }
        let name = node
            .get("name")
            .and_then(|name| name.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let value = node
            .get("value")
            .and_then(|value| value.get("value"))
            .and_then(Value::as_str);
        let name = truncate(name, MAX_ELEMENT_NAME_CHARS);
        let value = value
            .map(|value| truncate(value, MAX_ELEMENT_VALUE_CHARS))
            .filter(|value| !value.is_empty());

        // Nodes that are neither interactive nor named cannot be acted on and
        // cannot be described; keeping them only burns context.
        let interactive_role = is_interactive_role(&role);
        if name.is_empty() && !interactive_role {
            continue;
        }
        if STRUCTURAL_ROLES.contains(&role.as_str()) {
            continue;
        }

        let element = PrunedElement {
            backend_dom_node_id: node.get("backendDOMNodeId").and_then(Value::as_i64),
            role,
            name,
            value,
            disabled: node
                .get("properties")
                .and_then(Value::as_array)
                .map(|properties| {
                    properties.iter().any(|property| {
                        property.get("name").and_then(Value::as_str) == Some("disabled")
                            && property
                                .get("value")
                                .and_then(|value| value.get("value"))
                                .and_then(Value::as_bool)
                                .unwrap_or(false)
                    })
                })
                .unwrap_or(false),
        };
        if interactive_role {
            interactive.push(element);
        } else {
            static_content.push(element);
        }
    }

    let mut elements: Vec<PrunedElement> = Vec::with_capacity(max_elements);
    for element in interactive {
        if elements.len() >= max_elements {
            truncated = true;
            break;
        }
        elements.push(element);
    }
    if elements.len() < max_elements {
        for element in static_content {
            if elements.len() >= max_elements {
                truncated = true;
                break;
            }
            elements.push(element);
        }
    } else if elements.len() >= max_elements {
        truncated = true;
    }

    PrunedObservation {
        elements,
        truncated,
    }
}

fn node_depth(node: &Value, by_id: &HashMap<&str, &Value>) -> usize {
    let mut depth = 0usize;
    let mut current = node;
    // The tree is acyclic, but a malformed payload must not spin forever.
    for _ in 0..64 {
        let Some(parent_id) = current.get("parentId").and_then(Value::as_str) else {
            break;
        };
        let Some(parent) = by_id.get(parent_id) else {
            break;
        };
        depth += 1;
        current = parent;
    }
    depth
}

/// Assigns `r{generation}-{index}` references to pruned elements.
pub fn assign_references(elements: &[PrunedElement], generation: u64) -> Vec<BrowserElement> {
    elements
        .iter()
        .enumerate()
        .map(|(index, element)| BrowserElement {
            reference: format!("r{generation}-{}", index + 1),
            role: element.role.clone(),
            name: element.name.clone(),
            editable: element.editable(),
            value: element.value.clone(),
            disabled: element.disabled,
        })
        .collect()
}

/// Parses a `r{generation}-{index}` reference.
pub fn parse_reference(reference: &str) -> Option<(u64, usize)> {
    let rest = reference.strip_prefix('r')?;
    let (generation, index) = rest.split_once('-')?;
    let generation = generation.parse::<u64>().ok()?;
    let index = index.parse::<usize>().ok()?;
    Some((generation, index))
}

/// Why a reference could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceRejection {
    /// The reference is not in `r{generation}-{index}` form.
    Malformed,
    /// The reference came from an earlier observation and is stale.
    Stale,
    /// The index is beyond the elements of the current observation.
    OutOfRange,
}

impl ReferenceRejection {
    pub fn message(self, current_generation: u64) -> String {
        match self {
            Self::Malformed => {
                "the element reference is not in `r<generation>-<index>` form".to_string()
            }
            Self::Stale => format!(
                "the element reference is stale: the page has been observed again since it was issued \
                 (current generation is {current_generation}). Call browser_observe to get fresh refs."
            ),
            Self::OutOfRange => {
                "the element reference does not match any element in the current observation"
                    .to_string()
            }
        }
    }
}

/// Resolves a reference against the current generation and element list.
pub fn resolve_reference<'a>(
    reference: &str,
    current_generation: u64,
    elements: &'a [PrunedElement],
) -> Result<&'a PrunedElement, ReferenceRejection> {
    let Some((generation, index)) = parse_reference(reference) else {
        return Err(ReferenceRejection::Malformed);
    };
    if generation != current_generation {
        return Err(ReferenceRejection::Stale);
    }
    if index == 0 {
        return Err(ReferenceRejection::OutOfRange);
    }
    elements
        .get(index - 1)
        .ok_or(ReferenceRejection::OutOfRange)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, role: &str, name: &str, parent: Option<&str>) -> Value {
        let mut node = json!({
            "nodeId": id,
            "role": { "type": "role", "value": role },
            "name": { "type": "computedString", "value": name },
            "backendDOMNodeId": 100 + id.parse::<i64>().unwrap_or(0),
        });
        if let Some(parent) = parent {
            node["parentId"] = json!(parent);
        }
        node
    }

    #[test]
    fn interactive_elements_win_the_budget() {
        let nodes = vec![
            node("0", "RootWebArea", "Test page", None),
            node("1", "StaticText", "Some long paragraph", Some("0")),
            node("2", "button", "Submit", Some("0")),
            node("3", "textbox", "Email", Some("0")),
        ];
        let pruned = prune_ax_tree(&nodes, 2, 8);
        assert_eq!(pruned.elements.len(), 2);
        assert!(pruned.truncated);
        assert_eq!(pruned.elements[0].role, "button");
        assert_eq!(pruned.elements[1].role, "textbox");
    }

    #[test]
    fn unnamed_non_interactive_nodes_are_dropped() {
        let nodes = vec![
            node("0", "RootWebArea", "Test page", None),
            node("1", "generic", "", Some("0")),
            node("2", "StaticText", "", Some("0")),
            node("3", "button", "Go", Some("0")),
        ];
        let pruned = prune_ax_tree(&nodes, 50, 8);
        assert_eq!(pruned.elements.len(), 1);
        assert_eq!(pruned.elements[0].name, "Go");
    }

    #[test]
    fn depth_limit_excludes_deep_nodes() {
        let nodes = vec![
            node("0", "RootWebArea", "Test", None),
            node("1", "button", "One", Some("0")),
            node("2", "button", "Two", Some("1")),
            node("3", "button", "Three", Some("2")),
        ];
        let pruned = prune_ax_tree(&nodes, 50, 2);
        let names: Vec<&str> = pruned.elements.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["One", "Two"]);
    }

    #[test]
    fn long_names_are_truncated() {
        let long = "x".repeat(400);
        let nodes = vec![node("1", "button", &long, None)];
        let pruned = prune_ax_tree(&nodes, 10, 8);
        assert_eq!(
            pruned.elements[0].name.chars().count(),
            MAX_ELEMENT_NAME_CHARS
        );
        assert!(pruned.elements[0].name.ends_with('…'));
    }

    #[test]
    fn ignored_nodes_are_skipped() {
        let mut ignored = node("1", "button", "Hidden", None);
        ignored["ignored"] = json!(true);
        let nodes = vec![ignored, node("2", "button", "Visible", None)];
        let pruned = prune_ax_tree(&nodes, 10, 8);
        assert_eq!(pruned.elements.len(), 1);
        assert_eq!(pruned.elements[0].name, "Visible");
    }

    #[test]
    fn disabled_state_is_read_from_properties() {
        let mut disabled = node("1", "button", "Nope", None);
        disabled["properties"] = json!([
            { "name": "disabled", "value": { "type": "boolean", "value": true } }
        ]);
        let pruned = prune_ax_tree(&[disabled], 10, 8);
        assert!(pruned.elements[0].disabled);
    }

    #[test]
    fn element_budget_is_clamped_to_the_supported_range() {
        assert_eq!(
            clamp_max_elements(None),
            vibex_core::BROWSER_OBSERVE_DEFAULT_MAX_ELEMENTS
        );
        assert_eq!(
            clamp_max_elements(Some(1)),
            BROWSER_OBSERVE_MIN_MAX_ELEMENTS
        );
        assert_eq!(
            clamp_max_elements(Some(99_999)),
            BROWSER_OBSERVE_MAX_MAX_ELEMENTS
        );
        assert_eq!(clamp_max_elements(Some(120)), 120);
    }

    #[test]
    fn references_are_generation_scoped() {
        let elements = vec![PrunedElement {
            backend_dom_node_id: Some(7),
            role: "button".to_string(),
            name: "Submit".to_string(),
            value: None,
            disabled: false,
        }];
        let observed = assign_references(&elements, 3);
        assert_eq!(observed[0].reference, "r3-1");
        assert_eq!(parse_reference("r3-1"), Some((3, 1)));
        assert_eq!(parse_reference("nope"), None);

        assert_eq!(
            resolve_reference("r3-1", 3, &elements).unwrap().name,
            "Submit"
        );
        assert_eq!(
            resolve_reference("r2-1", 3, &elements).unwrap_err(),
            ReferenceRejection::Stale
        );
        assert_eq!(
            resolve_reference("r3-0", 3, &elements).unwrap_err(),
            ReferenceRejection::OutOfRange
        );
        assert_eq!(
            resolve_reference("r3-9", 3, &elements).unwrap_err(),
            ReferenceRejection::OutOfRange
        );
        assert_eq!(
            resolve_reference("garbage", 3, &elements).unwrap_err(),
            ReferenceRejection::Malformed
        );
    }

    #[test]
    fn stale_reference_message_tells_the_model_what_to_do() {
        let message = ReferenceRejection::Stale.message(9);
        assert!(message.contains("generation is 9"));
        assert!(message.contains("browser_observe"));
    }

    #[test]
    fn interactive_role_detection_is_case_insensitive() {
        assert!(is_interactive_role("Button"));
        assert!(is_interactive_role("textbox"));
        assert!(!is_interactive_role("StaticText"));
    }
}

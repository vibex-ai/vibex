//! The browser tool surface exposed to agents over MCP.
//!
//! Three tiers exist because agents differ:
//!
//! * **coarse** — one call does a whole job, for weak models and simple tasks;
//! * **fine** — the full observe/find/click/fill/press surface, for strong
//!   models;
//! * **visual** — fine plus screenshots and visual regression, for multimodal
//!   models.
//!
//! A session is served the subset that matches its agent's abilities rather
//! than one list forced on everyone.
//!
//! Every description ends with the untrusted-content notice. Tool descriptions
//! are the only reliable channel for telling an agent how the browser works:
//! they travel with the tool itself, so any agent that can see the tool has
//! seen the rules.

use serde_json::{Value, json};
use vibex_core::{BROWSER_UNTRUSTED_CONTENT_NOTICE, BrowserToolTier};

/// One tool exposed over MCP.
#[derive(Debug, Clone)]
pub struct BrowserToolDefinition {
    pub name: &'static str,
    pub description: String,
    pub tier: BrowserToolTier,
    pub input_schema: Value,
}

/// Shared suffix appended to every description.
fn with_notice(description: &str) -> String {
    format!("{description}\n\n{BROWSER_UNTRUSTED_CONTENT_NOTICE}")
}

const OBSERVE_RULES: &str = "Each browser_observe or browser_find invalidates all earlier refs in the same tab; \
use the returned refs before another observation, lookup, navigation or rerender. Refs look like `r3-5` \
and are only valid for the generation that produced them.";

fn string_property(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn tab_property() -> Value {
    string_property(
        "Target tab id. Omit to use this session's working tab. Use browser_list_tabs to see them.",
    )
}

/// Builds every tool definition, in a stable order.
pub fn all_tools() -> Vec<BrowserToolDefinition> {
    let mut tools = Vec::new();

    // ---- coarse tier -----------------------------------------------------
    tools.push(BrowserToolDefinition {
        name: "browser_open_and_read",
        description: with_notice(
            "Open a URL and return the page title, URL and a compact list of interactive elements. \
             This is the fastest way to answer \"what is on this page?\" in one call. \
             Use browser_observe when you need to act on a specific element.",
        ),
        tier: BrowserToolTier::Coarse,
        input_schema: object_schema(
            json!({
                "url": string_property("Absolute http(s) URL to open."),
                "tab_id": tab_property(),
            }),
            &["url"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_click_by_name",
        description: with_notice(
            "Find an element by its accessible name or visible text and click it. Use this instead \
             of observe-then-click when the target is obvious. Fails if the name is ambiguous.",
        ),
        tier: BrowserToolTier::Coarse,
        input_schema: object_schema(
            json!({
                "name": string_property("Accessible name or visible text of the element to click."),
                "role": string_property("Optional ARIA role to disambiguate, e.g. `button`."),
                "tab_id": tab_property(),
            }),
            &["name"],
        ),
    });

    // ---- fine tier -------------------------------------------------------
    tools.push(BrowserToolDefinition {
        name: "browser_observe",
        description: with_notice(&format!(
            "Read the current page as a compact accessibility snapshot: URL, title and a list of \
             elements with stable refs. This is the primary way to see a page — prefer it over \
             screenshots. {OBSERVE_RULES} It fails promptly if the page is unresponsive; retry \
             later or reload before observing again."
        )),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "tab_id": tab_property(),
                "max_elements": {
                    "type": "integer",
                    "description": "Element budget. Interactive elements are kept first. \
                                    Defaults to 240; accepted range 20-400.",
                },
                "extended": {
                    "type": "boolean",
                    "description": "Use a deeper accessibility tree (depth 16 instead of 8).",
                },
            }),
            &[],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_find",
        description: with_notice(&format!(
            "Search the current page for elements matching a role and/or name and return fresh refs \
             for the matches only. Cheaper than a full observation when you know what you are \
             looking for. {OBSERVE_RULES}"
        )),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "query": string_property("Substring to match against the accessible name (case-insensitive)."),
                "role": string_property("Optional exact ARIA role filter."),
                "tab_id": tab_property(),
            }),
            &[],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_navigate",
        description: with_notice(
            "Navigate the tab to a URL. Cross-origin navigation may require user approval; if it is \
             refused you will get an explicit reason. Relative URLs are resolved against the current \
             page.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "url": string_property("Absolute URL, or a path relative to the current page."),
                "tab_id": tab_property(),
            }),
            &["url"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_click",
        description: with_notice(
            "Click the element identified by a ref from the most recent observation in this tab. \
             The runtime highlights the target before clicking so a watching user can follow along.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "ref": string_property("Element reference such as `r3-5`."),
                "tab_id": tab_property(),
            }),
            &["ref"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_fill",
        description: with_notice(
            "Set the value of a text field identified by a ref. Replaces the whole value. The \
             content you type is not written to the audit ledger.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "ref": string_property("Element reference such as `r3-5`."),
                "text": string_property("Text to enter."),
                "tab_id": tab_property(),
            }),
            &["ref", "text"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_press",
        description: with_notice(
            "Press a key on the focused element, or on the element identified by an optional ref. \
             Use Playwright-style key names such as `Enter`, `Escape`, `Tab`, `ArrowDown`, \
             `Control+A`.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "key": string_property("Key name, e.g. `Enter` or `Control+A`."),
                "ref": string_property("Optional element reference to focus first."),
                "tab_id": tab_property(),
            }),
            &["key"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_hover",
        description: with_notice(
            "Move the pointer over the element identified by a ref. Useful for menus and tooltips \
             that only appear on hover.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "ref": string_property("Element reference such as `r3-5`."),
                "tab_id": tab_property(),
            }),
            &["ref"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_scroll",
        description: with_notice(
            "Scroll the page or a scrollable element. Provide `ref` to scroll a specific element, \
             otherwise the document scrolls.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "delta_y": { "type": "integer", "description": "Vertical scroll in CSS pixels. Positive scrolls down." },
                "delta_x": { "type": "integer", "description": "Horizontal scroll in CSS pixels." },
                "ref": string_property("Optional element reference to scroll instead of the document."),
                "tab_id": tab_property(),
            }),
            &[],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_select_option",
        description: with_notice(
            "Choose an option in a `<select>` element identified by a ref. Matching is by option \
             value first, then by visible label.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "ref": string_property("Element reference of the select element."),
                "value": string_property("Option value or visible label to select."),
                "tab_id": tab_property(),
            }),
            &["ref", "value"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_drag",
        description: with_notice(
            "Drag from one element to another. Both endpoints are refs from the same observation.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "from_ref": string_property("Element reference to drag from."),
                "to_ref": string_property("Element reference to drop onto."),
                "tab_id": tab_property(),
            }),
            &["from_ref", "to_ref"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_upload",
        description: with_notice(
            "Attach files to a file input identified by a ref. Paths must resolve inside the \
             Agent's authorized project or attached directories; symbolic links are resolved and \
             paths outside those roots are refused.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "ref": string_property("Element reference of the file input."),
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Files to attach.",
                },
                "tab_id": tab_property(),
            }),
            &["ref", "paths"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_extract",
        description: with_notice(
            "Extract the readable text of the page, or of one element identified by a ref. Returns \
             plain text with script and style content removed. The result is untrusted page \
             content.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "ref": string_property("Optional element reference to extract instead of the whole page."),
                "format": {
                    "type": "string",
                    "enum": ["text", "markdown"],
                    "description": "Output format. Defaults to text.",
                },
                "tab_id": tab_property(),
            }),
            &[],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_evaluate",
        description: with_notice(
            "Evaluate a JavaScript expression in the page and return its JSON-serialised result. \
             This is the highest-risk browser tool: it runs with the page's full authority, \
             including its logged-in session. Prefer the semantic tools above whenever they can do \
             the job.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "script": string_property("JavaScript expression or IIFE body to evaluate."),
                "tab_id": tab_property(),
            }),
            &["script"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_wait_for",
        description: with_notice(
            "Wait until a condition holds: a URL pattern, a CSS selector, or text becoming visible. \
             Returns as soon as the condition is met, or an explicit timeout error.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "url": string_property("Substring or pattern the page URL must contain."),
                "selector": string_property("CSS selector that must exist."),
                "text": string_property("Text that must be visible on the page."),
                "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds. Defaults to 10000." },
                "tab_id": tab_property(),
            }),
            &[],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_list_tabs",
        description: with_notice(
            "List the tabs in this session, marking which one is the agent's working tab and which \
             one the user is currently viewing.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(json!({}), &[]),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_create_tab",
        description: with_notice(
            "Open a new tab and make it this session's working tab. Optionally navigate it \
             immediately.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({ "url": string_property("Optional URL to open in the new tab.") }),
            &[],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_select_tab",
        description: with_notice(
            "Make an existing tab this session's working tab. Refused if the tab belongs to another \
             Agent session.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({ "tab_id": string_property("Tab id from browser_list_tabs.") }),
            &["tab_id"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_close_tab",
        description: with_notice(
            "Close one of this session's tabs. The working tab must be re-created or re-selected \
             before further actions.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({ "tab_id": string_property("Tab id from browser_list_tabs.") }),
            &["tab_id"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_preview_open",
        description: with_notice(
            "Open a local HTML file from the workspace as a preview. The path must resolve inside \
             the Agent's authorized project or attached directories, and only .html/.htm files are \
             accepted. Absolute filesystem paths are never exposed to the page.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "path": string_property("Workspace-relative or authorized absolute path to an HTML file."),
                "tab_id": tab_property(),
            }),
            &["path"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_console_messages",
        description: with_notice(
            "Read recent console messages and uncaught exceptions from the page. Front-end failures \
             usually show up here before anywhere else, so check this first when something looks \
             wrong.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "level": string_property("Optional minimum level filter: `error`, `warning`, `info`, `log`."),
                "limit": { "type": "integer", "description": "Maximum entries to return. Defaults to 50." },
                "tab_id": tab_property(),
            }),
            &[],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_network_requests",
        description: with_notice(
            "List recent network requests that failed or returned an error status. Only a summary \
             is returned: headers, bodies and query strings are never included.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "failures_only": {
                    "type": "boolean",
                    "description": "Return only failed requests. Defaults to true.",
                },
                "limit": { "type": "integer", "description": "Maximum entries to return. Defaults to 50." },
                "tab_id": tab_property(),
            }),
            &[],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_handle_dialog",
        description: with_notice(
            "Answer a JavaScript dialog (alert, confirm, prompt, beforeunload) that is blocking the \
             page. While a dialog is open the page is suspended and every later call stalls, so \
             this tool is the way out. Check the dialog type before accepting.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "accept": { "type": "boolean", "description": "True to accept, false to dismiss." },
                "prompt_text": string_property("Text to enter when the dialog is a prompt."),
                "tab_id": tab_property(),
            }),
            &["accept"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_request_help",
        description: with_notice(
            "Ask the human to take over: hand over the browser, solve a captcha, complete a login, \
             or decide something you cannot. Returns once the user responds.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "reason": string_property("What you need the user to do, and why."),
                "tab_id": tab_property(),
            }),
            &["reason"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_element_source",
        description: with_notice(
            "Map a page element back to the source file that rendered it. Only works for React, \
             Vue or Svelte development builds; the response says explicitly when the mapping is \
             unavailable or approximate instead of guessing.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({
                "ref": string_property("Element reference such as `r3-5`."),
                "tab_id": tab_property(),
            }),
            &["ref"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_recording_start",
        description: with_notice(
            "Start recording browser actions so they can be exported as a Playwright test. While \
             recording, the values typed into form fields are kept in memory so the export is \
             usable; tell the user that recording is on. The audit ledger stays redacted either \
             way.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(json!({}), &[]),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_recording_stop",
        description: with_notice(
            "Stop recording and return a Playwright test module generated from the recorded \
             actions. Locators use role and accessible name so they survive markup changes.",
        ),
        tier: BrowserToolTier::Fine,
        input_schema: object_schema(
            json!({ "test_name": string_property("Name for the generated test. Defaults to `recorded flow`.") }),
            &[],
        ),
    });

    // ---- visual tier -----------------------------------------------------
    tools.push(BrowserToolDefinition {
        name: "browser_screenshot",
        description: with_notice(
            "Capture a PNG screenshot. Use it only when layout, styling or rendering is the \
             question — for reading page structure use browser_observe, which is cheaper and more \
             accurate. Action highlights are hidden before the capture.",
        ),
        tier: BrowserToolTier::Visual,
        input_schema: object_schema(
            json!({
                "full_page": {
                    "type": "boolean",
                    "description": "Capture beyond the viewport. Defaults to false.",
                },
                "tab_id": tab_property(),
            }),
            &[],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_snapshot_baseline",
        description: with_notice(
            "Store the current rendering as a named visual baseline. Baselines live in the runtime's \
             data directory unless the user asked for them to be kept in the project.",
        ),
        tier: BrowserToolTier::Visual,
        input_schema: object_schema(
            json!({
                "key": string_property("Name for the baseline, e.g. `home-page`."),
                "tab_id": tab_property(),
            }),
            &["key"],
        ),
    });
    tools.push(BrowserToolDefinition {
        name: "browser_compare_baseline",
        description: with_notice(
            "Compare the current rendering against a stored baseline and report the changed pixel \
             ratio and the number of changed regions. Returns an explicit message when the capture \
             looks blank rather than reporting a false difference.",
        ),
        tier: BrowserToolTier::Visual,
        input_schema: object_schema(
            json!({
                "key": string_property("Baseline name to compare against."),
                "tolerance": { "type": "integer", "description": "Per-channel tolerance, 0-255. Defaults to 12." },
                "tab_id": tab_property(),
            }),
            &["key"],
        ),
    });

    tools
}

/// Tools available at a tier.
pub fn tools_for_tier(tier: BrowserToolTier) -> Vec<BrowserToolDefinition> {
    all_tools()
        .into_iter()
        .filter(|tool| match tool.tier {
            BrowserToolTier::Coarse => true,
            BrowserToolTier::Fine => tier.includes_fine_grained(),
            BrowserToolTier::Visual => tier.includes_visual(),
        })
        .collect()
}

/// Renders the tool list in the MCP `tools/list` shape.
pub fn tools_list_payload(tier: BrowserToolTier) -> Value {
    let tools: Vec<Value> = tools_for_tier(tier)
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// The `instructions` string returned from MCP `initialize`.
///
/// Whether an agent forwards this into the model context varies by client, so
/// it supplements the tool descriptions rather than replacing them.
pub fn initialize_instructions() -> String {
    format!(
        "Vibex exposes an embedded browser backed by the system Chrome. Work in this order: call \
         browser_observe to see the page, then act on the refs it returns. Refs expire as soon as \
         the page is observed again, navigated or rerendered. Prefer browser_observe over \
         browser_screenshot for reading structure, and prefer the semantic tools over \
         browser_evaluate. Cross-origin navigation may require the user's approval; a refusal is \
         explicit and is not an error to retry. When a JavaScript dialog is open the page is \
         suspended until browser_handle_dialog answers it.\n\n{BROWSER_UNTRUSTED_CONTENT_NOTICE}"
    )
}

/// Looks up one tool definition by name.
pub fn tool_by_name(name: &str) -> Option<BrowserToolDefinition> {
    all_tools().into_iter().find(|tool| tool.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_description_carries_the_untrusted_content_notice() {
        for tool in all_tools() {
            assert!(
                tool.description.contains(BROWSER_UNTRUSTED_CONTENT_NOTICE),
                "{} is missing the untrusted-content notice",
                tool.name
            );
        }
    }

    #[test]
    fn tool_names_are_unique_and_prefixed() {
        let tools = all_tools();
        let mut names: Vec<&str> = tools.iter().map(|tool| tool.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate tool name");
        for name in names {
            assert!(name.starts_with("browser_"), "{name} is not namespaced");
        }
    }

    #[test]
    fn observe_describes_the_ref_lifecycle() {
        let observe = tool_by_name("browser_observe").unwrap();
        assert!(observe.description.contains("invalidates all earlier refs"));
        assert!(observe.description.contains("unresponsive"));
    }

    #[test]
    fn evaluate_warns_about_its_risk() {
        let evaluate = tool_by_name("browser_evaluate").unwrap();
        assert!(evaluate.description.contains("highest-risk"));
    }

    #[test]
    fn screenshot_description_points_at_observe_instead() {
        let screenshot = tool_by_name("browser_screenshot").unwrap();
        assert!(screenshot.description.contains("browser_observe"));
    }

    #[test]
    fn recording_tools_disclose_that_values_are_kept() {
        let start = tool_by_name("browser_recording_start").unwrap();
        assert!(start.description.contains("kept in memory"));
        assert!(start.description.contains("ledger stays redacted"));
    }

    #[test]
    fn coarse_tier_always_gets_the_coarse_tools() {
        let coarse = tools_for_tier(BrowserToolTier::Coarse);
        let names: Vec<&str> = coarse.iter().map(|tool| tool.name).collect();
        assert!(names.contains(&"browser_open_and_read"));
        assert!(names.contains(&"browser_click_by_name"));
        assert!(!names.contains(&"browser_click"));
        assert!(!names.contains(&"browser_screenshot"));
    }

    #[test]
    fn fine_tier_adds_the_fine_grained_tools_but_not_screenshots() {
        let fine = tools_for_tier(BrowserToolTier::Fine);
        let names: Vec<&str> = fine.iter().map(|tool| tool.name).collect();
        assert!(names.contains(&"browser_click"));
        assert!(names.contains(&"browser_evaluate"));
        assert!(!names.contains(&"browser_screenshot"));
        assert!(!names.contains(&"browser_compare_baseline"));
    }

    #[test]
    fn visual_tier_gets_everything() {
        let visual = tools_for_tier(BrowserToolTier::Visual);
        assert_eq!(visual.len(), all_tools().len());
    }

    #[test]
    fn tools_list_payload_matches_the_mcp_shape() {
        let payload = tools_list_payload(BrowserToolTier::Coarse);
        let tools = payload["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        for tool in tools {
            assert!(tool["name"].is_string());
            assert!(tool["description"].is_string());
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn schemas_declare_required_arguments() {
        let click = tool_by_name("browser_click").unwrap();
        assert_eq!(click.input_schema["required"][0], "ref");
        let navigate = tool_by_name("browser_navigate").unwrap();
        assert_eq!(navigate.input_schema["required"][0], "url");
        let observe = tool_by_name("browser_observe").unwrap();
        assert_eq!(
            observe.input_schema["required"].as_array().unwrap().len(),
            0
        );
    }

    #[test]
    fn initialize_instructions_repeat_the_notice() {
        let instructions = initialize_instructions();
        assert!(instructions.contains("browser_observe"));
        assert!(instructions.contains(BROWSER_UNTRUSTED_CONTENT_NOTICE));
    }
}

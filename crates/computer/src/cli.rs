//! The `--computer-cli` entry point.
//!
//! This is the delivery path for an Agent that receives no built-in MCP server
//! at all (`pi` today). It runs the **same** MCP handler through the same
//! loopback endpoint: the command builds one JSON-RPC `tools/call`, posts it
//! with the session token the skill's environment carries, and prints the text
//! result. Nothing about policy, approval or the ledger is reimplemented here.
//!
//! What is different on this path is the *approval granularity*, and the honest
//! thing is to say so: the Agent runs a shell command, so the host can only
//! approve "run this command", not "click this button". The CLI therefore
//! prints the risk class and the approval decision the runtime made, so the
//! transcript still shows what the runtime refused.
//!
//! Screenshots are written to a private file and their base64 is stripped, so a
//! CLI transcript stays readable and small.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use vibex_core::unix_timestamp_ms;

use crate::error::{ComputerError, codes};
use crate::http::forward_to_endpoint;
use crate::mcp;
use crate::screenshot;

/// Endpoint environment variable, shared with the stdio sidecar.
pub const CLI_ENDPOINT_ENV: &str = "VIBEX_COMPUTER_MCP_ENDPOINT";
/// Token environment variable, shared with the stdio sidecar.
pub const CLI_TOKEN_ENV: &str = "VIBEX_COMPUTER_MCP_TOKEN";
/// Runtime home, used to place screenshot files.
pub const CLI_HOME_ENV: &str = "VIBEX_HOME";

/// One parsed CLI invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliInvocation {
    pub tool: String,
    pub arguments: Value,
}

/// The usage text the skill quotes.
pub const USAGE: &str = "\
vibex computer <command> [options]

Commands:
  apps                                   list applications on the desktop
  launch --app <id>                      start an installed application
  state --app <id> [--window <id>] [--screenshot] [--extended]
                                         read an application's accessibility tree
  click --app <id> (--element <ref> | --x <n> --y <n>) [--right] [--double]
                                         click an element or a point
  type --app <id> --text <text> [--element <ref>]
                                         enter text
  set --app <id> --element <ref> --value <value>
                                         write a value into one element
  key --app <id> --key <key> [--modifier <name>]...
                                         press a key or chord
  scroll --app <id> [--dx <n>] [--dy <n>]
                                         scroll inside a window
  permissions                            report the desktop permission state

Every command prints the runtime's verification line. `unverified` never means
success: observe the application again to check the effect. Approval for
destructive actions and foreground takeovers is requested inside the runtime,
and a refusal is printed as a refusal.";

/// Parses the CLI arguments, excluding the program name and the flag.
pub fn parse(arguments: &[String]) -> Result<CliInvocation, ComputerError> {
    let mut arguments = arguments.iter();
    let Some(command) = arguments.next() else {
        return Err(usage_error("a command is required"));
    };
    let rest: Vec<String> = arguments.cloned().collect();
    let flag = |name: &str| -> Option<String> {
        rest.iter()
            .position(|item| item == name)
            .and_then(|index| rest.get(index + 1).cloned())
    };
    let present = |name: &str| rest.iter().any(|item| item == name);
    let repeated = |name: &str| -> Vec<String> {
        let mut values = Vec::new();
        let mut index = 0;
        while index < rest.len() {
            if rest[index] == name
                && let Some(value) = rest.get(index + 1)
            {
                values.push(value.clone());
                index += 1;
            }
            index += 1;
        }
        values
    };
    let invocation = match command.as_str() {
        "apps" => CliInvocation {
            tool: crate::tools::names::LIST_APPS.to_string(),
            arguments: json!({}),
        },
        "launch" => CliInvocation {
            tool: crate::tools::names::LAUNCH_APP.to_string(),
            arguments: json!({ "app": require(&flag, "--app")? }),
        },
        "state" => {
            let mut arguments = json!({ "app": require(&flag, "--app")? });
            if let Some(window) = flag("--window") {
                arguments["window_id"] = json!(window);
            }
            if present("--screenshot") {
                arguments["include_screenshot"] = json!(true);
            }
            if present("--extended") {
                arguments["extended"] = json!(true);
            }
            CliInvocation {
                tool: crate::tools::names::GET_APP_STATE.to_string(),
                arguments,
            }
        }
        "click" => {
            let mut arguments = json!({ "app": require(&flag, "--app")? });
            if let Some(element) = flag("--element") {
                arguments["element"] = json!(element);
            }
            if let (Some(x), Some(y)) = (flag("--x"), flag("--y")) {
                arguments["x"] = json!(parse_number(&x, "--x")?);
                arguments["y"] = json!(parse_number(&y, "--y")?);
            }
            if arguments.get("element").is_none() && arguments.get("x").is_none() {
                return Err(usage_error(
                    "click needs `--element` or both `--x` and `--y`",
                ));
            }
            if present("--right") {
                arguments["button"] = json!("right");
            }
            if present("--double") {
                arguments["click_count"] = json!(2);
            }
            CliInvocation {
                tool: crate::tools::names::CLICK.to_string(),
                arguments,
            }
        }
        "type" => {
            let mut arguments = json!({
                "app": require(&flag, "--app")?,
                "text": require(&flag, "--text")?,
            });
            if let Some(element) = flag("--element") {
                arguments["element"] = json!(element);
            }
            CliInvocation {
                tool: crate::tools::names::TYPE_TEXT.to_string(),
                arguments,
            }
        }
        "set" => CliInvocation {
            tool: crate::tools::names::SET_VALUE.to_string(),
            arguments: json!({
                "app": require(&flag, "--app")?,
                "element": require(&flag, "--element")?,
                "value": require(&flag, "--value")?,
            }),
        },
        "key" => {
            let mut arguments = json!({
                "app": require(&flag, "--app")?,
                "key": require(&flag, "--key")?,
            });
            let modifiers = repeated("--modifier");
            if !modifiers.is_empty() {
                arguments["modifiers"] = json!(modifiers);
            }
            CliInvocation {
                tool: crate::tools::names::PRESS_KEY.to_string(),
                arguments,
            }
        }
        "scroll" => {
            let mut arguments = json!({ "app": require(&flag, "--app")? });
            if let Some(dx) = flag("--dx") {
                arguments["delta_x"] = json!(parse_number(&dx, "--dx")?);
            }
            if let Some(dy) = flag("--dy") {
                arguments["delta_y"] = json!(parse_number(&dy, "--dy")?);
            }
            CliInvocation {
                tool: crate::tools::names::SCROLL.to_string(),
                arguments,
            }
        }
        "permissions" => CliInvocation {
            tool: crate::tools::names::PERMISSIONS.to_string(),
            arguments: json!({}),
        },
        other => {
            return Err(usage_error(&format!("`{other}` is not a computer command")));
        }
    };
    Ok(invocation)
}

fn require(flag: &impl Fn(&str) -> Option<String>, name: &str) -> Result<String, ComputerError> {
    flag(name).ok_or_else(|| usage_error(&format!("`{name}` is required")))
}

fn parse_number(value: &str, name: &str) -> Result<f64, ComputerError> {
    value
        .parse::<f64>()
        .map_err(|_| usage_error(&format!("`{name}` must be a number")))
}

fn usage_error(message: &str) -> ComputerError {
    ComputerError::validation("computer_cli_usage", message.to_string())
        .with_recovery_hint(USAGE.split('\n').next().unwrap_or_default())
}

/// Runs one CLI invocation against the runtime endpoint.
pub fn run(arguments: &[String]) -> Result<String, ComputerError> {
    let invocation = parse(arguments)?;
    let endpoint = std::env::var(CLI_ENDPOINT_ENV).map_err(|_| {
        ComputerError::capability(
            codes::FEATURE_DISABLED,
            format!("{CLI_ENDPOINT_ENV} is not set: the runtime did not hand this shell a computer endpoint"),
        )
        .with_recovery_hint(
            "Enable computer use for this Agent in Vibex; the skill is only useful while the \
             runtime is running.",
        )
    })?;
    let token = std::env::var(CLI_TOKEN_ENV).map_err(|_| {
        ComputerError::capability(
            codes::FEATURE_DISABLED,
            format!("{CLI_TOKEN_ENV} is not set"),
        )
    })?;
    let home = std::env::var(CLI_HOME_ENV)
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("vibex-computer"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| ComputerError::process("computer_cli_runtime", error.to_string()))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| ComputerError::process("computer_cli_client", error.to_string()))?;
    let mut message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": invocation.tool, "arguments": invocation.arguments },
    });
    let response = runtime
        .block_on(forward_to_endpoint(&client, &endpoint, &token, &message))?
        .ok_or_else(|| {
            ComputerError::process(
                "computer_cli_no_response",
                "the runtime accepted the call but returned no result",
            )
        })?;
    let _ = &mut message;
    render_response(&response, &home)
}

/// Renders an MCP response for a terminal.
///
/// Image blocks become a path; text blocks are printed as they are. The result
/// is what the Agent reads, so the verification line must survive.
fn render_response(response: &Value, home: &Path) -> Result<String, ComputerError> {
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the runtime refused the call");
        return Err(ComputerError::process(
            "computer_cli_refused",
            message.to_string(),
        ));
    }
    let result = response.get("result").cloned().unwrap_or(Value::Null);
    let mut lines = Vec::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        lines.push(text.to_string());
                    }
                }
                Some("image") => {
                    let data = block
                        .get("data")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let mime = block
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("image/png");
                    match screenshot::write_screenshot_file(home, "cli", data, unix_timestamp_ms())
                    {
                        Ok(written) => lines.push(format!(
                            "[screenshot written to {} ({} bytes, expires {})]",
                            written.path.display(),
                            written.byte_len,
                            written.expires_at_ms
                        )),
                        Err(error) => lines.push(format!(
                            "[screenshot could not be written: {error}; mime {mime}]"
                        )),
                    }
                }
                _ => {}
            }
        }
    }
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        lines.push("The runtime reported this call as an error.".to_string());
    }
    if lines.is_empty() {
        lines.push("The runtime returned no content.".to_string());
    }
    Ok(lines.join("\n"))
}

/// Builds the JSON-RPC message for one invocation. Exposed for tests.
pub fn message_for(invocation: &CliInvocation) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": invocation.tool, "arguments": invocation.arguments },
    })
}

/// The token the skill's environment carries, for diagnostics.
pub fn token_prefix(token: &str) -> String {
    let prefix = mcp::COMPUTER_TOKEN_PREFIX;
    if token.starts_with(prefix) {
        prefix.to_string()
    } else {
        "<unknown>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn commands_map_onto_the_same_tools_the_mcp_server_exposes() {
        let invocation = parse(&args(&["apps"])).unwrap();
        assert_eq!(invocation.tool, crate::tools::names::LIST_APPS);
        let invocation = parse(&args(&["launch", "--app", "com.example.notes"])).unwrap();
        assert_eq!(invocation.tool, crate::tools::names::LAUNCH_APP);
        assert_eq!(invocation.arguments["app"], "com.example.notes");
        let invocation = parse(&args(&["state", "--app", "com.example.notes"])).unwrap();
        assert_eq!(invocation.tool, crate::tools::names::GET_APP_STATE);
        assert_eq!(invocation.arguments["app"], "com.example.notes");
        let invocation = parse(&args(&[
            "click",
            "--app",
            "notes",
            "--element",
            "c1-3",
            "--double",
        ]))
        .unwrap();
        assert_eq!(invocation.arguments["element"], "c1-3");
        assert_eq!(invocation.arguments["click_count"], 2);
        let invocation = parse(&args(&[
            "key",
            "--app",
            "notes",
            "--key",
            "s",
            "--modifier",
            "cmd",
        ]))
        .unwrap();
        assert_eq!(invocation.arguments["modifiers"][0], "cmd");
        let invocation = parse(&args(&[
            "set",
            "--app",
            "notes",
            "--element",
            "c1-1",
            "--value",
            "hi",
        ]))
        .unwrap();
        assert_eq!(invocation.arguments["value"], "hi");
        let invocation = parse(&args(&["scroll", "--app", "notes", "--dy", "-120"])).unwrap();
        assert_eq!(invocation.arguments["delta_y"], -120.0);
        assert_eq!(
            parse(&args(&["permissions"])).unwrap().tool,
            crate::tools::names::PERMISSIONS
        );
    }

    #[test]
    fn a_click_needs_a_target_and_the_error_names_the_alternative() {
        let error = parse(&args(&["click", "--app", "notes"])).unwrap_err();
        assert_eq!(error.code, "computer_cli_usage");
        assert!(error.message.contains("--element"));
    }

    #[test]
    fn a_missing_option_is_a_usage_error_not_a_panic() {
        assert!(parse(&args(&["type", "--app", "notes"])).is_err());
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["teleport"])).is_err());
        assert!(parse(&args(&["click", "--app", "n", "--x", "one", "--y", "2"])).is_err());
    }

    #[test]
    fn an_image_block_becomes_a_path_without_its_base64() {
        let home = tempfile::tempdir().unwrap();
        let response = json!({
            "result": {
                "content": [
                    { "type": "text", "text": "observed" },
                    { "type": "image", "data": "QUJD", "mimeType": "image/png" }
                ],
                "isError": false
            }
        });
        let rendered = render_response(&response, home.path()).unwrap();
        assert!(rendered.contains("observed"));
        assert!(rendered.contains("screenshot written to"));
        assert!(
            !rendered.contains("QUJD"),
            "the base64 must not reach the transcript"
        );
    }

    #[test]
    fn a_refusal_keeps_the_reason() {
        let home = tempfile::tempdir().unwrap();
        let response = json!({ "error": { "code": -32000, "message": "the user denied it" } });
        let error = render_response(&response, home.path()).unwrap_err();
        assert!(error.message.contains("denied"));
    }

    #[test]
    fn only_a_computer_token_prefix_is_reported() {
        assert_eq!(token_prefix("ctok_session_x"), "ctok");
        assert_eq!(token_prefix("btok_session_x"), "<unknown>");
    }
}

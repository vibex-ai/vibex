//! The `--browser-mcp` stdio sidecar.
//!
//! Agents that cannot use HTTP MCP still need a delivery path. For those, the
//! runtime advertises a stdio server whose command is the Vibex binary with
//! `--browser-mcp`, and this module is what that flag runs.
//!
//! Two constraints shape it, both consequences of how stdio MCP works:
//!
//! 1. **The sidecar is spawned by the third-party agent CLI, not by Vibex.**
//!    There is no child handle, no supervisor, no restart, and its stderr
//!    belongs to the agent. It therefore holds no state: every message is
//!    forwarded to the runtime's loopback endpoint, where the actual browser
//!    service lives.
//! 2. **One malformed message must not kill the process.** The runtime's own
//!    delegation sidecar exits on a bad frame; that failure mode would take the
//!    whole browser tool surface down with it, so this bridge answers with a
//!    JSON-RPC error and keeps reading.

use std::io::BufReader;
use std::time::Duration;

use serde_json::{Value, json};

use crate::http::forward_to_endpoint;
use crate::mcp::{read_stdio_message, write_stdio_message};

/// Environment variable carrying the runtime MCP endpoint URL.
pub const BROWSER_MCP_ENDPOINT_ENV: &str = "VIBEX_BROWSER_MCP_ENDPOINT";
/// Environment variable carrying the session-scoped bearer token.
pub const BROWSER_MCP_TOKEN_ENV: &str = "VIBEX_BROWSER_MCP_TOKEN";

/// Runs the stdio bridge until stdin closes.
///
/// Returns `Err` only for a fatal setup problem; per-message failures are
/// reported to the client and the loop continues.
pub fn run_browser_mcp_stdio() -> Result<(), String> {
    let endpoint = std::env::var(BROWSER_MCP_ENDPOINT_ENV)
        .map_err(|_| format!("{BROWSER_MCP_ENDPOINT_ENV} is not set"))?;
    let token = std::env::var(BROWSER_MCP_TOKEN_ENV)
        .map_err(|_| format!("{BROWSER_MCP_TOKEN_ENV} is not set"))?;
    if endpoint.trim().is_empty() || token.trim().is_empty() {
        return Err("the browser MCP endpoint and token must not be empty".to_string());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start the browser MCP runtime: {error}"))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|error| format!("failed to build the browser MCP HTTP client: {error}"))?;

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    loop {
        let message = match read_stdio_message(&mut reader) {
            Ok(Some(message)) => message,
            Ok(None) => return Ok(()),
            Err(error) => {
                // A read error is the one case where the transport itself is
                // broken; there is nothing left to answer on.
                return Err(format!("failed to read a browser MCP message: {error}"));
            }
        };
        let value: Value = match serde_json::from_slice(&message) {
            Ok(value) => value,
            Err(error) => {
                // Keep serving: a single bad frame is the client's problem, not
                // a reason to take the tool surface down.
                let _ = write_stdio_message(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("parse error: {error}") },
                    }),
                );
                continue;
            }
        };
        let is_notification = value.get("id").is_none();
        let response = runtime.block_on(forward_to_endpoint(&client, &endpoint, &token, &value));
        match response {
            Ok(Some(response)) => {
                if let Err(error) = write_stdio_message(&mut writer, &response) {
                    return Err(format!("failed to write a browser MCP response: {error}"));
                }
            }
            Ok(None) => {
                // Notifications are acknowledged by the endpoint with 202 and
                // produce no reply.
                let _ = is_notification;
            }
            Err(error) => {
                if is_notification {
                    continue;
                }
                let id = value.get("id").cloned().unwrap_or(Value::Null);
                let _ = write_stdio_message(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32000, "message": error.message },
                    }),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_variable_names_are_stable() {
        assert_eq!(BROWSER_MCP_ENDPOINT_ENV, "VIBEX_BROWSER_MCP_ENDPOINT");
        assert_eq!(BROWSER_MCP_TOKEN_ENV, "VIBEX_BROWSER_MCP_TOKEN");
    }

    #[test]
    fn a_parse_error_produces_a_json_rpc_error_frame() {
        let mut buffer = Vec::new();
        write_stdio_message(
            &mut buffer,
            &json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32700, "message": "parse error" },
            }),
        )
        .unwrap();
        let text = String::from_utf8(buffer).unwrap();
        assert!(text.ends_with('\n'));
        let parsed: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(parsed["error"]["code"], -32700);
    }

    #[test]
    fn the_bridge_reads_both_framings() {
        // Newline-delimited.
        let mut reader = BufReader::new(std::io::Cursor::new(b"{\"id\":1}\n".to_vec()));
        assert!(read_stdio_message(&mut reader).unwrap().is_some());
        // Content-Length.
        let payload = b"{\"id\":2}";
        let framed: Vec<u8> = format!("Content-Length: {}\r\n\r\n", payload.len())
            .into_bytes()
            .into_iter()
            .chain(payload.iter().copied())
            .collect();
        let mut reader = BufReader::new(std::io::Cursor::new(framed));
        assert!(read_stdio_message(&mut reader).unwrap().is_some());
    }
}

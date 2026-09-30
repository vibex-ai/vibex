//! The `--computer-mcp` stdio sidecar.
//!
//! Agents that cannot use HTTP MCP still need a delivery path. For those the
//! runtime advertises a stdio server whose command is the Vibex binary with
//! `--computer-mcp`, and this module is what that flag runs.
//!
//! Two constraints shape it, both consequences of how stdio MCP works:
//!
//! 1. **The sidecar is spawned by the third-party agent CLI, not by Vibex.** It
//!    therefore holds no state: every message is forwarded to the runtime's
//!    loopback endpoint, where the actual service — and every policy decision —
//!    lives. A sidecar that made its own decisions would be an unapproved path
//!    to the desktop.
//! 2. **One malformed message must not kill the process.** The bridge answers
//!    with a JSON-RPC error and keeps reading.

use std::io::BufReader;
use std::time::Duration;

use serde_json::{Value, json};

use crate::http::forward_to_endpoint;
use crate::mcp::{read_stdio_message, write_stdio_message};

/// Environment variable carrying the runtime MCP endpoint URL.
pub const COMPUTER_MCP_ENDPOINT_ENV: &str = "VIBEX_COMPUTER_MCP_ENDPOINT";
/// Environment variable carrying the session-scoped bearer token.
pub const COMPUTER_MCP_TOKEN_ENV: &str = "VIBEX_COMPUTER_MCP_TOKEN";

/// Runs the stdio bridge until stdin closes.
pub fn run_computer_mcp_stdio() -> Result<(), String> {
    let endpoint = std::env::var(COMPUTER_MCP_ENDPOINT_ENV)
        .map_err(|_| format!("{COMPUTER_MCP_ENDPOINT_ENV} is not set"))?;
    let token = std::env::var(COMPUTER_MCP_TOKEN_ENV)
        .map_err(|_| format!("{COMPUTER_MCP_TOKEN_ENV} is not set"))?;
    if endpoint.trim().is_empty() || token.trim().is_empty() {
        return Err("the computer MCP endpoint and token must not be empty".to_string());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start the computer MCP runtime: {error}"))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| format!("failed to build the computer MCP HTTP client: {error}"))?;

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    loop {
        let message = match read_stdio_message(&mut reader) {
            Ok(Some(message)) => message,
            Ok(None) => return Ok(()),
            Err(error) => {
                return Err(format!("failed to read a computer MCP message: {error}"));
            }
        };
        let value: Value = match serde_json::from_slice(&message) {
            Ok(value) => value,
            Err(error) => {
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
        let response =
            match runtime.block_on(forward_to_endpoint(&client, &endpoint, &token, &value)) {
                Ok(response) => response,
                Err(error) => {
                    if is_notification {
                        // A notification has no reply; there is nothing to report
                        // the failure on.
                        continue;
                    }
                    Some(json!({
                        "jsonrpc": "2.0",
                        "id": value.get("id").cloned().unwrap_or(Value::Null),
                        "error": { "code": -32000, "message": error.message },
                    }))
                }
            };
        if let Some(response) = response
            && let Err(error) = write_stdio_message(&mut writer, &response)
        {
            return Err(format!("failed to write a computer MCP message: {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_env_names_are_stable() {
        // The runtime writes these into the descriptor; the sidecar reads them.
        // Drifting one side is how every browser tool call once answered 401.
        assert_eq!(COMPUTER_MCP_ENDPOINT_ENV, "VIBEX_COMPUTER_MCP_ENDPOINT");
        assert_eq!(COMPUTER_MCP_TOKEN_ENV, "VIBEX_COMPUTER_MCP_TOKEN");
    }
}

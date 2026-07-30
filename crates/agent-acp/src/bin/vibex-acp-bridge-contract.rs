use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};
use vibex_agent_acp::{
    AcpAdapterHealthReport, AcpBridgeContractAdapterReport, AcpBridgeContractRunner,
    AcpCompatibilityRegistry, BridgeContractMcpFixture, ManagedAcpAdapterStore,
};
use vibex_core::unix_timestamp_ms;

const MCP_FIXTURE_MODE: &str = "--mcp-fixture-server";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeVersionContractReport {
    schema_version: u32,
    generated_at_ms: i64,
    status: &'static str,
    managed_root: String,
    workspace_root: String,
    adapters: Vec<AdapterReport>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdapterReport {
    agent_id: String,
    adapter_id: String,
    status: &'static str,
    health: Option<AcpAdapterHealthReport>,
    contract: Option<AcpBridgeContractAdapterReport>,
    error_code: Option<String>,
    error_category: Option<String>,
}

#[derive(Debug)]
struct Arguments {
    managed_root: PathBuf,
    workspace_root: PathBuf,
    cleanup_workspace_root: bool,
    output: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    if env::args().nth(1).as_deref() == Some(MCP_FIXTURE_MODE) {
        if let Err(error) = run_mcp_fixture_server() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    let result = run().await;
    if let Err(error) = result {
        eprintln!("{}: {}", error.code, error.message);
        std::process::exit(1);
    }
}

async fn run() -> vibex_core::VibexResult<()> {
    let arguments = parse_arguments()?;
    let managed_root = create_and_resolve_root(
        &arguments.managed_root,
        "acp_bridge_contract_root_create_failed",
        "acp_bridge_contract_root_resolve_failed",
        "ACP bridge contract managed root",
    )?;
    let workspace_root = create_and_resolve_root(
        &arguments.workspace_root,
        "acp_bridge_contract_workspace_create_failed",
        "acp_bridge_contract_workspace_resolve_failed",
        "ACP bridge contract workspace root",
    )?;
    let _workspace_guard = arguments
        .cleanup_workspace_root
        .then(|| DisposableWorkspaceGuard::new(workspace_root.clone()));
    let fixture_command = env::current_exe().map_err(|error| {
        vibex_core::VibexError::process(
            "acp_bridge_contract_executable_missing",
            "ACP bridge contract executable path could not be resolved",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let store = ManagedAcpAdapterStore::new(managed_root)?;
    let registry = AcpCompatibilityRegistry::builtin()?;
    let runner = AcpBridgeContractRunner::default();

    let mut adapters = Vec::new();
    for descriptor in registry.descriptors() {
        let adapter_workspace = workspace_root.join(descriptor.adapter_id.as_str());
        fs::create_dir_all(&adapter_workspace).map_err(|error| {
            vibex_core::VibexError::storage(
                "acp_bridge_contract_adapter_workspace_create_failed",
                "ACP bridge contract adapter workspace could not be created",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let mcp_fixture = BridgeContractMcpFixture {
            command: fixture_command.clone(),
            args: vec![MCP_FIXTURE_MODE.to_string()],
            marker_path: adapter_workspace.join("mcp-initialized.marker"),
        };
        let result = async {
            let installation = store.install(descriptor).await?;
            let health = store.health_probe(descriptor).await?;
            let contract = runner
                .run(descriptor, &installation, &adapter_workspace, &mcp_fixture)
                .await?;
            Ok::<_, vibex_core::VibexError>((health, contract))
        }
        .await;
        match result {
            Ok((health, contract)) => {
                let status = if contract.summary.gate_passed {
                    "passed"
                } else {
                    "failed"
                };
                adapters.push(AdapterReport {
                    agent_id: descriptor.agent_id.to_string(),
                    adapter_id: descriptor.adapter_id.to_string(),
                    status,
                    health: Some(health),
                    contract: Some(contract),
                    error_code: None,
                    error_category: None,
                });
            }
            Err(error) => adapters.push(AdapterReport {
                agent_id: descriptor.agent_id.to_string(),
                adapter_id: descriptor.adapter_id.to_string(),
                status: "blocked",
                health: None,
                contract: None,
                error_code: Some(error.code),
                error_category: Some(format!("{:?}", error.category).to_ascii_lowercase()),
            }),
        }
    }

    let status = if adapters.iter().all(|adapter| adapter.status == "passed") {
        "passed"
    } else if adapters.iter().any(|adapter| adapter.status == "blocked") {
        "blocked"
    } else {
        "failed"
    };
    let report = BridgeVersionContractReport {
        schema_version: 2,
        generated_at_ms: unix_timestamp_ms(),
        status,
        managed_root: "[managed-temp-root]".to_string(),
        workspace_root: "[disposable-workspace]".to_string(),
        adapters,
    };
    write_report(arguments.output, &report)?;
    if status != "passed" {
        return Err(vibex_core::VibexError::provider(
            "acp_bridge_contract_gate_failed",
            "At least one fixed ACP adapter failed the bridge version contract",
        ));
    }
    Ok(())
}

fn create_and_resolve_root(
    path: &PathBuf,
    create_code: &str,
    resolve_code: &str,
    label: &str,
) -> vibex_core::VibexResult<PathBuf> {
    fs::create_dir_all(path).map_err(|error| {
        vibex_core::VibexError::storage(create_code, format!("{label} could not be created"))
            .with_diagnostic("error", error.to_string())
    })?;
    path.canonicalize().map_err(|error| {
        vibex_core::VibexError::storage(resolve_code, format!("{label} could not be resolved"))
            .with_diagnostic("error", error.to_string())
    })
}

fn write_report(
    output: Option<PathBuf>,
    report: &BridgeVersionContractReport,
) -> vibex_core::VibexResult<()> {
    let bytes = serde_json::to_vec_pretty(report).map_err(|error| {
        vibex_core::VibexError::validation(
            "acp_bridge_contract_report_encode_failed",
            "ACP bridge contract report could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    if let Some(output) = output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                vibex_core::VibexError::storage(
                    "acp_bridge_contract_output_root_create_failed",
                    "ACP bridge contract output directory could not be created",
                )
                .with_diagnostic("error", error.to_string())
            })?;
        }
        fs::write(output, &bytes).map_err(|error| {
            vibex_core::VibexError::storage(
                "acp_bridge_contract_report_write_failed",
                "ACP bridge contract report could not be written",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    } else {
        println!("{}", String::from_utf8_lossy(&bytes));
    }
    Ok(())
}

fn parse_arguments() -> vibex_core::VibexResult<Arguments> {
    let base = env::temp_dir().join("vibex-acp-bridge-contract");
    let mut managed_root = base.join("adapters");
    let mut workspace_root = base.join(format!(
        "workspaces-{}-{}",
        std::process::id(),
        unix_timestamp_ms()
    ));
    let mut cleanup_workspace_root = true;
    let mut output = None;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--managed-root" => {
                managed_root = required_path_argument(&mut arguments, "--managed-root")?;
            }
            "--workspace-root" => {
                workspace_root = required_path_argument(&mut arguments, "--workspace-root")?;
                cleanup_workspace_root = false;
            }
            "--output" => {
                output = Some(required_path_argument(&mut arguments, "--output")?);
            }
            _ => {
                return Err(vibex_core::VibexError::validation(
                    "acp_bridge_contract_argument_unknown",
                    "Unknown ACP bridge contract argument",
                )
                .with_diagnostic("argument", argument));
            }
        }
    }
    if !managed_root.is_absolute() || !workspace_root.is_absolute() {
        return Err(vibex_core::VibexError::validation(
            "acp_bridge_contract_root_relative",
            "ACP bridge contract managed and workspace roots must be absolute",
        ));
    }
    Ok(Arguments {
        managed_root,
        workspace_root,
        cleanup_workspace_root,
        output,
    })
}

struct DisposableWorkspaceGuard {
    path: PathBuf,
}

impl DisposableWorkspaceGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for DisposableWorkspaceGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn required_path_argument(
    arguments: &mut impl Iterator<Item = String>,
    name: &str,
) -> vibex_core::VibexResult<PathBuf> {
    arguments.next().map(PathBuf::from).ok_or_else(|| {
        vibex_core::VibexError::validation(
            "acp_bridge_contract_argument_missing",
            format!("{name} requires a path"),
        )
    })
}

fn run_mcp_fixture_server() -> Result<(), String> {
    let marker = env::var_os("VIBEX_ACP_CONTRACT_MCP_MARKER")
        .map(PathBuf::from)
        .ok_or_else(|| "MCP fixture marker path is missing".to_string())?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in BufReader::new(stdin.lock()).lines() {
        let line = line.map_err(|error| error.to_string())?;
        let request: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request.get("method").and_then(Value::as_str);
        let result = match method {
            Some("initialize") => {
                fs::write(&marker, b"initialized\n").map_err(|error| error.to_string())?;
                let requested_version = request
                    .get("params")
                    .and_then(|params| params.get("protocolVersion"))
                    .cloned()
                    .unwrap_or_else(|| Value::String("2025-06-18".to_string()));
                json!({
                    "protocolVersion": requested_version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "vibex-contract-fixture", "version": "1.0.0" }
                })
            }
            Some("tools/list") => json!({
                "tools": [{
                    "name": "contract_marker",
                    "description": "Returns a deterministic contract marker",
                    "inputSchema": { "type": "object", "properties": {} }
                }]
            }),
            Some("tools/call") => json!({
                "content": [{ "type": "text", "text": "contract-marker" }],
                "isError": false
            }),
            Some("resources/list") => json!({ "resources": [] }),
            Some("prompts/list") => json!({ "prompts": [] }),
            Some("ping") => json!({}),
            _ => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": "fixture method unavailable" }
                });
                writeln!(stdout, "{response}").map_err(|error| error.to_string())?;
                stdout.flush().map_err(|error| error.to_string())?;
                continue;
            }
        };
        let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        writeln!(stdout, "{response}").map_err(|error| error.to_string())?;
        stdout.flush().map_err(|error| error.to_string())?;
    }
    Ok(())
}

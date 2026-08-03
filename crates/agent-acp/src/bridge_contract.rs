//! Explicit real-adapter implementation of the section 25.2 bridge contract.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use vibex_core::{VibexError, VibexResult};

use crate::managed_adapter::VerifiedAcpAdapterInstallation;
use crate::protocol::{
    AcpOperation, build_initialize_params, build_session_cancel_params, build_session_load_params,
    build_session_new_params, build_session_prompt_params, decode_incoming,
};
use crate::registry::{
    AcpAgentCompatibility, BridgeContractCaseResult, BridgeContractEvidenceKind,
    BridgeContractRequirement, BridgeContractStatus, BridgeContractSummary,
};
use crate::runtime::{PARENT_SESSION_ENV_KEYS, PROBE_ENV};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
const DEFAULT_PROMPT_TIMEOUT: Duration = Duration::from_secs(3 * 60);
const MCP_MARKER_TIMEOUT: Duration = Duration::from_secs(20);
const CANCEL_DELAY: Duration = Duration::from_millis(300);

#[derive(Debug, Clone)]
pub struct BridgeContractMcpFixture {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub marker_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpBridgeContractAdapterReport {
    pub adapter_id: String,
    pub adapter_version: String,
    pub compatibility_identity: String,
    pub binary_identity: String,
    pub cases: Vec<BridgeContractCaseResult>,
    pub summary: BridgeContractSummary,
}

#[derive(Debug, Clone)]
pub struct AcpBridgeContractRunner {
    request_timeout: Duration,
    prompt_timeout: Duration,
}

impl Default for AcpBridgeContractRunner {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            prompt_timeout: DEFAULT_PROMPT_TIMEOUT,
        }
    }
}

impl AcpBridgeContractRunner {
    pub async fn run(
        &self,
        descriptor: &AcpAgentCompatibility,
        installation: &VerifiedAcpAdapterInstallation,
        workspace: &Path,
        mcp_fixture: &BridgeContractMcpFixture,
    ) -> VibexResult<AcpBridgeContractAdapterReport> {
        validate_contract_workspace(workspace)?;
        let mut results = initial_case_results(descriptor);
        let mut connection = match BridgeConnection::spawn(installation).await {
            Ok(connection) => connection,
            Err(error) => {
                set_case_error(&mut results, "initialize", false, error.code);
                return finalize_report(descriptor, installation, results);
            }
        };

        let execution = self
            .run_connected(
                descriptor,
                &connection,
                workspace,
                mcp_fixture,
                &mut results,
            )
            .await;
        connection.shutdown().await;
        if let Err(error) = execution {
            tracing::debug!(
                target: "vibex_agent_acp",
                adapter_id = %descriptor.adapter_id,
                error_code = %error.code,
                "ACP bridge contract stopped after a failed prerequisite"
            );
        }
        finalize_report(descriptor, installation, results)
    }

    async fn run_connected(
        &self,
        descriptor: &AcpAgentCompatibility,
        connection: &BridgeConnection,
        workspace: &Path,
        mcp_fixture: &BridgeContractMcpFixture,
        results: &mut BTreeMap<String, BridgeContractCaseResult>,
    ) -> VibexResult<()> {
        let (initialize_call, initialize_duration) = connection
            .timed_request(
                AcpOperation::Initialize,
                build_initialize_params(false, false, false, false, false),
                self.request_timeout,
            )
            .await;
        set_case_duration(results, "initialize", initialize_duration);
        let initialize = match initialize_call {
            Ok(result) => {
                set_case_pass(results, "initialize", true);
                result
            }
            Err(error) => {
                set_case_error(results, "initialize", true, error.code.clone());
                return Err(error);
            }
        };
        let capabilities = RuntimeCapabilities::from_initialize(&initialize);

        if mcp_fixture.marker_path.exists() {
            let _ = fs::remove_file(&mcp_fixture.marker_path);
        }
        let mcp_servers = json!([{
            "name": "vibex-contract-fixture",
            "command": mcp_fixture.command.to_string_lossy(),
            "args": mcp_fixture.args,
            "env": [{
                "name": "VIBEX_ACP_CONTRACT_MCP_MARKER",
                "value": mcp_fixture.marker_path.to_string_lossy(),
            }],
        }]);
        let (session_new_call, session_new_duration) = connection
            .timed_request(
                AcpOperation::SessionNew,
                build_session_new_params(workspace, mcp_servers.clone()),
                self.request_timeout,
            )
            .await;
        set_case_duration(results, "session_new", session_new_duration);
        let session_new = match session_new_call {
            Ok(result) => {
                set_case_pass(results, "session_new", true);
                result
            }
            Err(error) => {
                set_case_error(results, "session_new", true, error.code.clone());
                return Err(error);
            }
        };
        let session_id = match session_new.get("sessionId").and_then(Value::as_str) {
            Some(session_id) => session_id.to_string(),
            None => {
                let error = VibexError::provider(
                    "acp_bridge_contract_session_id_missing",
                    "ACP bridge contract session/new returned no session id",
                );
                set_case_error(results, "session_new", true, error.code.clone());
                return Err(error);
            }
        };

        self.verify_mode_and_config(connection, &session_id, &session_new, results)
            .await;
        self.verify_set_model(descriptor, connection, &session_id, &session_new, results)
            .await;

        let attachment_path = workspace.join("bridge-contract-attachment.txt");
        fs::write(&attachment_path, b"vibex bridge contract attachment\n").map_err(|error| {
            VibexError::storage(
                "acp_bridge_contract_attachment_write_failed",
                "ACP bridge contract attachment fixture could not be written",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let prompt = vec![
            json!({
                "type": "text",
                "text": "Reply with the single word OK after considering the attached marker. Do not edit files."
            }),
            json!({
                "type": "resource_link",
                "uri": format!("file://{}", attachment_path.display()),
                "name": "bridge-contract-attachment.txt"
            }),
        ];
        let (prompt_call, prompt_duration) = connection
            .timed_request(
                AcpOperation::SessionPrompt,
                build_session_prompt_params(&session_id, prompt),
                self.prompt_timeout,
            )
            .await;
        set_case_duration(results, "session_prompt", prompt_duration);
        set_case_duration(results, "attachments", prompt_duration);
        match prompt_call {
            Ok(_) => {
                set_case_pass(results, "session_prompt", true);
                set_case_pass(results, "attachments", true);
            }
            Err(error) => {
                set_case_error(results, "session_prompt", true, error.code.clone());
                set_case_error(results, "attachments", true, error.code);
            }
        }

        let mcp_started = Instant::now();
        if wait_for_marker(&mcp_fixture.marker_path, MCP_MARKER_TIMEOUT).await {
            set_case_duration(results, "mcp", elapsed_ms(mcp_started));
            set_case_pass(results, "mcp", true);
        } else {
            set_case_duration(results, "mcp", elapsed_ms(mcp_started));
            set_case_error(
                results,
                "mcp",
                true,
                "acp_bridge_contract_mcp_not_initialized".to_string(),
            );
        }

        self.verify_permission(connection, &session_id, &session_new, results)
            .await;
        self.verify_cancel(connection, &session_id, results).await;
        self.verify_resume_and_load(
            connection,
            workspace,
            &session_id,
            mcp_servers.clone(),
            &capabilities,
            results,
        )
        .await;
        self.verify_fork(
            connection,
            workspace,
            &session_id,
            mcp_servers,
            &capabilities,
            results,
        )
        .await;
        Ok(())
    }

    async fn verify_mode_and_config(
        &self,
        connection: &BridgeConnection,
        session_id: &str,
        session_new: &Value,
        results: &mut BTreeMap<String, BridgeContractCaseResult>,
    ) {
        let mode_id = session_new
            .get("modes")
            .and_then(|modes| modes.get("currentModeId"))
            .and_then(Value::as_str);
        match mode_id {
            Some(mode_id) => set_result_from_timed_call(
                results,
                "session_set_mode",
                true,
                connection
                    .timed_request(
                        AcpOperation::SessionSetMode,
                        json!({ "sessionId": session_id, "modeId": mode_id }),
                        self.request_timeout,
                    )
                    .await,
            ),
            None => set_case_error(
                results,
                "session_set_mode",
                false,
                "acp_bridge_contract_mode_not_advertised".to_string(),
            ),
        }

        let config_option = session_new
            .get("configOptions")
            .and_then(Value::as_array)
            .and_then(|options| {
                options.iter().find(|option| {
                    option.get("id").and_then(Value::as_str) != Some("mode")
                        && option.get("currentValue").is_some()
                })
            });
        match config_option {
            Some(option) => {
                let config_id = option.get("id").and_then(Value::as_str).unwrap_or_default();
                let value = option.get("currentValue").cloned().unwrap_or(Value::Null);
                let mut params = json!({
                    "sessionId": session_id,
                    "configId": config_id,
                    "value": value,
                });
                if params.get("value").is_some_and(Value::is_boolean) {
                    params["type"] = Value::String("boolean".to_string());
                }
                set_result_from_timed_call(
                    results,
                    "session_set_config_option",
                    true,
                    connection
                        .timed_request(
                            AcpOperation::SessionSetConfigOption,
                            params,
                            self.request_timeout,
                        )
                        .await,
                );
            }
            None => set_case_error(
                results,
                "session_set_config_option",
                false,
                "acp_bridge_contract_config_not_advertised".to_string(),
            ),
        }
    }

    async fn verify_set_model(
        &self,
        descriptor: &AcpAgentCompatibility,
        connection: &BridgeConnection,
        session_id: &str,
        session_new: &Value,
        results: &mut BTreeMap<String, BridgeContractCaseResult>,
    ) {
        let advertised = descriptor
            .known_quirks
            .iter()
            .any(|quirk| quirk.operation == Some(AcpOperation::SessionSetModel));
        if !advertised {
            set_case_not_advertised(results, "session_set_model");
            return;
        }
        let model_id = session_new
            .get("models")
            .and_then(|models| models.get("currentModelId"))
            .and_then(Value::as_str)
            .or_else(|| {
                session_new
                    .get("configOptions")
                    .and_then(Value::as_array)
                    .and_then(|options| {
                        options.iter().find(|option| {
                            option.get("id").and_then(Value::as_str) == Some("model")
                        })
                    })
                    .and_then(|option| option.get("currentValue"))
                    .and_then(Value::as_str)
            });
        match model_id {
            Some(model_id) => set_result_from_timed_call(
                results,
                "session_set_model",
                true,
                connection
                    .timed_request(
                        AcpOperation::SessionSetModel,
                        json!({ "sessionId": session_id, "modelId": model_id }),
                        self.request_timeout,
                    )
                    .await,
            ),
            None => set_case_error(
                results,
                "session_set_model",
                true,
                "acp_bridge_contract_model_missing".to_string(),
            ),
        }
    }

    async fn verify_permission(
        &self,
        connection: &BridgeConnection,
        session_id: &str,
        session_new: &Value,
        results: &mut BTreeMap<String, BridgeContractCaseResult>,
    ) {
        let restrictive_mode = session_new
            .get("modes")
            .and_then(|modes| modes.get("availableModes"))
            .and_then(Value::as_array)
            .and_then(|modes| {
                ["read-only", "default", "plan"]
                    .into_iter()
                    .find_map(|candidate| {
                        modes
                            .iter()
                            .find(|mode| mode.get("id").and_then(Value::as_str) == Some(candidate))
                            .map(|_| candidate)
                    })
            });
        if let Some(mode_id) = restrictive_mode {
            let _ = connection
                .request(
                    AcpOperation::SessionSetMode,
                    json!({ "sessionId": session_id, "modeId": mode_id }),
                    self.request_timeout,
                )
                .await;
        }

        let before = connection.permission_requests();
        let (call, duration_ms) = connection
            .timed_request(
                AcpOperation::SessionPrompt,
                build_session_prompt_params(
                    session_id,
                    vec![json!({
                        "type": "text",
                        "text": "Use the shell tool to create permission-contract-marker.txt in the current workspace. Do not use another approach."
                    })],
                ),
                self.prompt_timeout,
            )
            .await;
        set_case_duration(results, "permission", duration_ms);
        let observed = connection.permission_requests() > before;
        match (observed, call) {
            (true, Ok(_)) => set_case_pass(results, "permission", true),
            (true, Err(error)) => {
                set_case_pass(results, "permission", true);
                tracing::debug!(
                    target: "vibex_agent_acp",
                    error_code = %error.code,
                    "permission contract observed; prompt ended with provider error"
                );
            }
            (false, Ok(_)) => set_case_error(
                results,
                "permission",
                true,
                "acp_bridge_contract_permission_not_observed".to_string(),
            ),
            (false, Err(error)) => set_case_error(results, "permission", true, error.code),
        }
    }

    async fn verify_cancel(
        &self,
        connection: &BridgeConnection,
        session_id: &str,
        results: &mut BTreeMap<String, BridgeContractCaseResult>,
    ) {
        let started = Instant::now();
        let pending = connection
            .start_request(
                AcpOperation::SessionPrompt,
                build_session_prompt_params(
                    session_id,
                    vec![json!({
                        "type": "text",
                        "text": "Write a detailed ten-part explanation of software architecture."
                    })],
                ),
            )
            .await;
        let pending = match pending {
            Ok(pending) => pending,
            Err(error) => {
                set_case_duration(results, "session_cancel", elapsed_ms(started));
                set_case_error(results, "session_cancel", true, error.code);
                return;
            }
        };
        sleep(CANCEL_DELAY).await;
        if let Err(error) = connection
            .notify(
                AcpOperation::SessionCancel,
                build_session_cancel_params(session_id),
            )
            .await
        {
            set_case_duration(results, "session_cancel", elapsed_ms(started));
            set_case_error(results, "session_cancel", true, error.code);
            return;
        }
        let call = connection.wait_request(pending, self.prompt_timeout).await;
        set_case_duration(results, "session_cancel", elapsed_ms(started));
        set_result_from_call(results, "session_cancel", true, call);
    }

    async fn verify_resume_and_load(
        &self,
        connection: &BridgeConnection,
        workspace: &Path,
        session_id: &str,
        mcp_servers: Value,
        capabilities: &RuntimeCapabilities,
        results: &mut BTreeMap<String, BridgeContractCaseResult>,
    ) {
        if capabilities.resume {
            set_result_from_timed_call(
                results,
                "session_resume",
                true,
                connection
                    .timed_request(
                        AcpOperation::SessionResume,
                        json!({
                            "sessionId": session_id,
                            "cwd": workspace.display().to_string(),
                            "mcpServers": mcp_servers.clone(),
                        }),
                        self.request_timeout,
                    )
                    .await,
            );
        } else {
            set_case_error(
                results,
                "session_resume",
                false,
                "acp_bridge_contract_resume_not_advertised".to_string(),
            );
        }

        if capabilities.load {
            set_result_from_timed_call(
                results,
                "session_load",
                true,
                connection
                    .timed_request(
                        AcpOperation::SessionLoad,
                        build_session_load_params(session_id, workspace, mcp_servers),
                        self.request_timeout,
                    )
                    .await,
            );
        } else {
            set_case_error(
                results,
                "session_load",
                false,
                "acp_bridge_contract_load_not_advertised".to_string(),
            );
        }
    }

    async fn verify_fork(
        &self,
        connection: &BridgeConnection,
        workspace: &Path,
        session_id: &str,
        mcp_servers: Value,
        capabilities: &RuntimeCapabilities,
        results: &mut BTreeMap<String, BridgeContractCaseResult>,
    ) {
        if !capabilities.fork {
            set_case_not_advertised(results, "session_fork");
            return;
        }
        set_result_from_timed_call(
            results,
            "session_fork",
            true,
            connection
                .timed_request(
                    AcpOperation::SessionFork,
                    json!({
                        "sessionId": session_id,
                        "cwd": workspace.display().to_string(),
                        "mcpServers": mcp_servers,
                    }),
                    self.request_timeout,
                )
                .await,
        );
    }
}

#[derive(Debug, Default)]
struct RuntimeCapabilities {
    load: bool,
    resume: bool,
    fork: bool,
}

impl RuntimeCapabilities {
    fn from_initialize(initialize: &Value) -> Self {
        let capabilities = initialize.get("agentCapabilities");
        let sessions = capabilities.and_then(|value| value.get("sessionCapabilities"));
        Self {
            load: capabilities
                .and_then(|value| value.get("loadSession"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            resume: sessions.and_then(|value| value.get("resume")).is_some(),
            fork: sessions.and_then(|value| value.get("fork")).is_some(),
        }
    }
}

struct PendingRequest {
    id: u64,
    receiver: oneshot::Receiver<Value>,
}

struct BridgeConnection {
    child: Child,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_request_id: AtomicU64,
    permission_requests: Arc<AtomicU64>,
    reader_task: JoinHandle<()>,
    stderr_task: JoinHandle<()>,
}

impl BridgeConnection {
    async fn spawn(installation: &VerifiedAcpAdapterInstallation) -> VibexResult<Self> {
        let mut command = Command::new(&installation.command.program);
        command
            .args(&installation.command.args)
            .current_dir(&installation.command.current_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for key in PARENT_SESSION_ENV_KEYS {
            command.env_remove(key);
        }
        for (key, value) in PROBE_ENV {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|error| {
            VibexError::process(
                "acp_bridge_contract_spawn_failed",
                "ACP bridge contract adapter process could not be started",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let stdin = Arc::new(AsyncMutex::new(child.stdin.take().ok_or_else(|| {
            VibexError::process(
                "acp_bridge_contract_stdio_unavailable",
                "ACP bridge contract adapter stdin is unavailable",
            )
        })?));
        let stdout = child.stdout.take().ok_or_else(|| {
            VibexError::process(
                "acp_bridge_contract_stdio_unavailable",
                "ACP bridge contract adapter stdout is unavailable",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            VibexError::process(
                "acp_bridge_contract_stdio_unavailable",
                "ACP bridge contract adapter stderr is unavailable",
            )
        })?;
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let permission_requests = Arc::new(AtomicU64::new(0));
        let reader_task = tokio::spawn(read_adapter_output(
            stdout,
            Arc::clone(&stdin),
            Arc::clone(&pending),
            Arc::clone(&permission_requests),
        ));
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(_line)) = lines.next_line().await {}
        });
        Ok(Self {
            child,
            stdin,
            pending,
            next_request_id: AtomicU64::new(1),
            permission_requests,
            reader_task,
            stderr_task,
        })
    }

    async fn request(
        &self,
        operation: AcpOperation,
        params: Value,
        request_timeout: Duration,
    ) -> VibexResult<Value> {
        let pending = self.start_request(operation, params).await?;
        self.wait_request(pending, request_timeout).await
    }

    async fn timed_request(
        &self,
        operation: AcpOperation,
        params: Value,
        request_timeout: Duration,
    ) -> (VibexResult<Value>, u64) {
        let started = Instant::now();
        let result = self.request(operation, params, request_timeout).await;
        (result, elapsed_ms(started))
    }

    async fn start_request(
        &self,
        operation: AcpOperation,
        params: Value,
    ) -> VibexResult<PendingRequest> {
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| contract_lock_error())?
            .insert(id, sender);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": operation.method(),
            "params": params,
        });
        if let Err(error) = write_json_line(&self.stdin, &message).await {
            self.pending
                .lock()
                .map_err(|_| contract_lock_error())?
                .remove(&id);
            return Err(error);
        }
        Ok(PendingRequest { id, receiver })
    }

    async fn wait_request(
        &self,
        pending: PendingRequest,
        request_timeout: Duration,
    ) -> VibexResult<Value> {
        let message = match timeout(request_timeout, pending.receiver).await {
            Ok(Ok(message)) => message,
            Ok(Err(_)) => {
                return Err(VibexError::process(
                    "acp_bridge_contract_process_closed",
                    "ACP bridge contract process closed before a response",
                ));
            }
            Err(_) => {
                if let Ok(mut requests) = self.pending.lock() {
                    requests.remove(&pending.id);
                }
                return Err(VibexError::process(
                    "acp_bridge_contract_request_timeout",
                    "ACP bridge contract request timed out",
                ));
            }
        };
        if let Some(error) = message.get("error") {
            let code = error.get("code").and_then(Value::as_i64);
            return Err(if code == Some(-32601) {
                VibexError::capability(
                    "acp_bridge_contract_method_not_found",
                    "ACP bridge contract operation is not implemented",
                )
            } else if rpc_error_is_blocked(error) {
                VibexError::provider(
                    "acp_bridge_contract_provider_blocked",
                    "ACP bridge contract is blocked by provider availability or authentication",
                )
            } else {
                VibexError::provider(
                    "acp_bridge_contract_rpc_failed",
                    "ACP bridge contract operation returned an error",
                )
            });
        }
        message.get("result").cloned().ok_or_else(|| {
            VibexError::provider(
                "acp_bridge_contract_response_shape_invalid",
                "ACP bridge contract response has no result",
            )
        })
    }

    async fn notify(&self, operation: AcpOperation, params: Value) -> VibexResult<()> {
        write_json_line(
            &self.stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": operation.method(),
                "params": params,
            }),
        )
        .await
    }

    fn permission_requests(&self) -> u64 {
        self.permission_requests.load(Ordering::Relaxed)
    }

    async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        self.reader_task.abort();
        self.stderr_task.abort();
    }
}

fn rpc_error_is_blocked(error: &Value) -> bool {
    let lower = error.to_string().to_ascii_lowercase();
    [
        "auth",
        "credential",
        "login",
        "unauthorized",
        "forbidden",
        "rate limit",
        "rate_limit",
        "service unavailable",
        "temporarily unavailable",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

async fn read_adapter_output<R>(
    stdout: R,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    permission_requests: Arc<AtomicU64>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let _decoded = decode_incoming(&message);
        if message.get("method").is_none() {
            let id = message.get("id").and_then(Value::as_u64);
            if let Some(id) = id
                && let Ok(mut requests) = pending.lock()
                && let Some(sender) = requests.remove(&id)
            {
                let _ = sender.send(message);
            }
            continue;
        }
        if message.get("id").is_none() {
            continue;
        }
        let method = message.get("method").and_then(Value::as_str);
        let response = if method == Some(AcpOperation::PermissionRequest.method()) {
            permission_requests.fetch_add(1, Ordering::Relaxed);
            permission_denied_response(&message)
        } else {
            json!({
                "jsonrpc": "2.0",
                "id": message.get("id").cloned().unwrap_or(Value::Null),
                "error": { "code": -32601, "message": "contract host method unavailable" }
            })
        };
        let _ = write_json_line(&stdin, &response).await;
    }
}

fn permission_denied_response(request: &Value) -> Value {
    let option_id = request
        .get("params")
        .and_then(|params| params.get("options"))
        .and_then(Value::as_array)
        .and_then(|options| {
            options.iter().find(|option| {
                option
                    .get("kind")
                    .or_else(|| option.get("type"))
                    .or_else(|| option.get("name"))
                    .and_then(Value::as_str)
                    .is_some_and(|kind| {
                        let kind = kind.to_ascii_lowercase().replace(['-', ' '], "_");
                        kind.contains("reject") || kind.contains("deny")
                    })
            })
        })
        .and_then(|option| {
            option
                .get("optionId")
                .or_else(|| option.get("id"))
                .and_then(Value::as_str)
        });
    let outcome = option_id.map_or_else(
        || json!({ "outcome": "cancelled" }),
        |option_id| json!({ "outcome": "selected", "optionId": option_id }),
    );
    json!({
        "jsonrpc": "2.0",
        "id": request.get("id").cloned().unwrap_or(Value::Null),
        "result": { "outcome": outcome }
    })
}

async fn write_json_line(stdin: &Arc<AsyncMutex<ChildStdin>>, message: &Value) -> VibexResult<()> {
    let mut bytes = serde_json::to_vec(message).map_err(|error| {
        VibexError::validation(
            "acp_bridge_contract_request_encode_failed",
            "ACP bridge contract request could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    bytes.push(b'\n');
    let mut stdin = stdin.lock().await;
    stdin.write_all(&bytes).await.map_err(|error| {
        VibexError::process(
            "acp_bridge_contract_write_failed",
            "ACP bridge contract request could not be written",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    stdin.flush().await.map_err(|error| {
        VibexError::process(
            "acp_bridge_contract_write_failed",
            "ACP bridge contract request could not be flushed",
        )
        .with_diagnostic("error", error.to_string())
    })
}

fn initial_case_results(
    descriptor: &AcpAgentCompatibility,
) -> BTreeMap<String, BridgeContractCaseResult> {
    descriptor
        .bridge_contract
        .iter()
        .map(|case| {
            (
                case.id.clone(),
                BridgeContractCaseResult {
                    case_id: case.id.clone(),
                    advertised: false,
                    status: if case.requirement == BridgeContractRequirement::NotApplicable {
                        BridgeContractStatus::NotAdvertised
                    } else {
                        BridgeContractStatus::Blocked
                    },
                    duration_ms: 0,
                    error_code: Some("acp_bridge_contract_not_run".to_string()),
                },
            )
        })
        .collect()
}

fn set_case_pass(
    results: &mut BTreeMap<String, BridgeContractCaseResult>,
    case_id: &str,
    advertised: bool,
) {
    if let Some(result) = results.get_mut(case_id) {
        result.advertised = advertised;
        result.status = BridgeContractStatus::Passed;
        result.error_code = None;
    }
}

fn set_case_not_advertised(
    results: &mut BTreeMap<String, BridgeContractCaseResult>,
    case_id: &str,
) {
    if let Some(result) = results.get_mut(case_id) {
        result.advertised = false;
        result.status = BridgeContractStatus::NotAdvertised;
        result.error_code = None;
    }
}

fn set_case_duration(
    results: &mut BTreeMap<String, BridgeContractCaseResult>,
    case_id: &str,
    duration_ms: u64,
) {
    if let Some(result) = results.get_mut(case_id) {
        result.duration_ms = duration_ms;
    }
}

fn set_case_error(
    results: &mut BTreeMap<String, BridgeContractCaseResult>,
    case_id: &str,
    advertised: bool,
    error_code: String,
) {
    if let Some(result) = results.get_mut(case_id) {
        result.advertised = advertised;
        result.status = if error_code.contains("auth") || error_code.contains("_blocked") {
            BridgeContractStatus::Blocked
        } else {
            BridgeContractStatus::Failed
        };
        result.error_code = Some(error_code);
    }
}

fn set_result_from_timed_call(
    results: &mut BTreeMap<String, BridgeContractCaseResult>,
    case_id: &str,
    advertised: bool,
    call: (VibexResult<Value>, u64),
) {
    let (call, duration_ms) = call;
    set_case_duration(results, case_id, duration_ms);
    set_result_from_call(results, case_id, advertised, call);
}

fn set_result_from_call(
    results: &mut BTreeMap<String, BridgeContractCaseResult>,
    case_id: &str,
    advertised: bool,
    call: VibexResult<Value>,
) {
    match call {
        Ok(_) => set_case_pass(results, case_id, advertised),
        Err(error) => set_case_error(results, case_id, advertised, error.code),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn finalize_report(
    descriptor: &AcpAgentCompatibility,
    installation: &VerifiedAcpAdapterInstallation,
    results: BTreeMap<String, BridgeContractCaseResult>,
) -> VibexResult<AcpBridgeContractAdapterReport> {
    let cases: Vec<_> = descriptor
        .bridge_contract
        .iter()
        .filter_map(|case| results.get(&case.id).cloned())
        .collect();
    let summary = BridgeContractSummary::evaluate(
        &descriptor.bridge_contract,
        &cases,
        BridgeContractEvidenceKind::RealManagedAdapter,
    )?;
    Ok(AcpBridgeContractAdapterReport {
        adapter_id: descriptor.adapter_id.to_string(),
        adapter_version: installation.adapter_version.to_string(),
        compatibility_identity: installation.compatibility_identity.to_string(),
        binary_identity: installation.binary_identity.clone(),
        cases,
        summary,
    })
}

async fn wait_for_marker(path: &Path, wait: Duration) -> bool {
    timeout(wait, async {
        loop {
            if path.is_file() {
                return;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .is_ok()
}

fn validate_contract_workspace(workspace: &Path) -> VibexResult<()> {
    if !workspace.is_absolute() || !workspace.is_dir() {
        return Err(VibexError::validation(
            "acp_bridge_contract_workspace_invalid",
            "ACP bridge contract workspace must be an existing absolute directory",
        ));
    }
    Ok(())
}

fn contract_lock_error() -> VibexError {
    VibexError::provider(
        "acp_bridge_contract_lock_poisoned",
        "ACP bridge contract pending request state could not be locked",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AcpCompatibilityRegistry, CLAUDE_AGENT_ID};
    use tempfile::TempDir;
    use vibex_core::{AcpAdapterId, AgentId};

    #[test]
    fn permission_denial_prefers_reject_option() {
        let response = permission_denied_response(&json!({
            "id": 9,
            "params": {
                "options": [
                    { "optionId": "allow", "kind": "allow_once" },
                    { "optionId": "reject", "kind": "reject-once" }
                ]
            }
        }));
        assert_eq!(response["result"]["outcome"]["outcome"], "selected");
        assert_eq!(response["result"]["outcome"]["optionId"], "reject");
    }

    #[test]
    fn capability_parser_only_uses_explicit_initialize_fields() {
        let capabilities = RuntimeCapabilities::from_initialize(&json!({
            "agentCapabilities": {
                "loadSession": true,
                "sessionCapabilities": { "resume": {}, "fork": {} }
            }
        }));
        assert!(capabilities.load);
        assert!(capabilities.resume);
        assert!(capabilities.fork);
        assert!(!RuntimeCapabilities::from_initialize(&json!({})).resume);
    }

    #[test]
    fn unfinished_real_report_fails_gate() {
        let descriptor = AcpCompatibilityRegistry::builtin()
            .unwrap()
            .for_agent(&AgentId::parse(CLAUDE_AGENT_ID).unwrap())
            .unwrap()
            .clone();
        let installation = VerifiedAcpAdapterInstallation {
            adapter_id: AcpAdapterId::parse("claude-agent-acp").unwrap(),
            adapter_version: descriptor.distribution.exact_version.clone(),
            compatibility_identity: descriptor.expected_compatibility_identity(),
            binary_identity: "sha256:test".to_string(),
            runtime_versions: BTreeMap::new(),
            install_root: PathBuf::from("/tmp/test"),
            command: crate::ManagedAdapterCommand {
                program: PathBuf::from("node"),
                args: Vec::new(),
                current_dir: PathBuf::from("/tmp"),
            },
        };
        let report = finalize_report(
            &descriptor,
            &installation,
            initial_case_results(&descriptor),
        )
        .unwrap();
        assert!(!report.summary.gate_passed);
        assert!(
            report
                .summary
                .failed_cases
                .contains(&"initialize".to_string())
        );
    }

    #[tokio::test]
    async fn marker_wait_is_bounded() {
        let temp = TempDir::new().unwrap();
        assert!(!wait_for_marker(&temp.path().join("missing"), Duration::from_millis(5)).await);
    }
}

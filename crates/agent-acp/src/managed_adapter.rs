//! Isolated managed npm installation and short-lived ACP health probes.
//!
//! This module deliberately does not own long-lived ACP processes. P2-03's
//! Process Registry consumes the verified command produced here.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;
use vibex_core::{AcpAdapterId, VibexError, VibexResult};

use crate::private_fs::{
    ensure_private_runtime_directory, ensure_private_runtime_file,
    write_private_runtime_file_atomic,
};
use crate::process_environment::sanitize_inherited_appimage_environment;
use crate::protocol::{AcpOperation, build_initialize_params, decode_incoming};
use crate::registry::{
    AcpAgentCompatibility, AdapterCompatibilityIdentity, ManagedRuntimeDependency,
    is_safe_managed_path_segment,
};
use crate::runtime::{PARENT_SESSION_ENV_KEYS, PROBE_ENV};

const DEFAULT_INSTALL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const DIAGNOSTIC_LIMIT_BYTES: usize = 2 * 1024;
static STAGING_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedAdapterCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub current_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAcpAdapterInstallation {
    pub adapter_id: AcpAdapterId,
    pub adapter_version: Version,
    pub compatibility_identity: AdapterCompatibilityIdentity,
    pub binary_identity: String,
    pub runtime_versions: BTreeMap<String, Version>,
    pub install_root: PathBuf,
    pub command: ManagedAdapterCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAdapterHealthReport {
    pub adapter_id: String,
    pub adapter_version: String,
    pub compatibility_identity: String,
    pub binary_identity: String,
    pub node_version: String,
    pub reported_adapter_version: String,
    pub protocol_version: Option<String>,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
}

#[derive(Clone)]
pub struct ManagedAcpAdapterStore {
    root: PathBuf,
    node_command: PathBuf,
    installer: Arc<dyn NpmInstallExecutor>,
    probe_timeout: Duration,
}

impl ManagedAcpAdapterStore {
    pub fn new(root: impl Into<PathBuf>) -> VibexResult<Self> {
        Self::with_commands(root, "npm", "node")
    }

    pub fn with_commands(
        root: impl Into<PathBuf>,
        npm_command: impl Into<PathBuf>,
        node_command: impl Into<PathBuf>,
    ) -> VibexResult<Self> {
        let root = root.into();
        validate_managed_root(&root)?;
        Ok(Self {
            root,
            node_command: node_command.into(),
            installer: Arc::new(SystemNpmInstallExecutor {
                npm_command: npm_command.into(),
                timeout: DEFAULT_INSTALL_TIMEOUT,
            }),
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
        })
    }

    #[cfg(test)]
    fn with_installer(
        root: impl Into<PathBuf>,
        node_command: impl Into<PathBuf>,
        installer: Arc<dyn NpmInstallExecutor>,
        probe_timeout: Duration,
    ) -> VibexResult<Self> {
        let root = root.into();
        validate_managed_root(&root)?;
        Ok(Self {
            root,
            node_command: node_command.into(),
            installer,
            probe_timeout,
        })
    }

    pub fn installation_root(&self, descriptor: &AcpAgentCompatibility) -> VibexResult<PathBuf> {
        if !is_safe_managed_path_segment(descriptor.adapter_id.as_str()) {
            return Err(VibexError::validation(
                "acp_managed_adapter_id_path_invalid",
                "Managed ACP adapter id must be one safe path segment",
            )
            .with_diagnostic("adapterId", descriptor.adapter_id.to_string()));
        }
        Ok(self
            .root
            .join(descriptor.adapter_id.as_str())
            .join(descriptor.distribution.exact_version.to_string()))
    }

    /// Install one descriptor-owned package tree without touching npm's global
    /// prefix. Existing verified versions are returned idempotently.
    pub async fn install(
        &self,
        descriptor: &AcpAgentCompatibility,
    ) -> VibexResult<VerifiedAcpAdapterInstallation> {
        let target = self.installation_root(descriptor)?;
        if target.exists() {
            return self.inspect_at(descriptor, &target);
        }

        let adapter_root = target.parent().ok_or_else(|| {
            VibexError::validation(
                "acp_managed_install_root_invalid",
                "Managed ACP adapter version directory has no parent",
            )
        })?;
        ensure_private_runtime_directory(&self.root)?;
        ensure_private_runtime_directory(adapter_root)?;

        let staging = adapter_root.join(format!(
            ".staging-{}-{}-{}",
            descriptor.distribution.exact_version,
            std::process::id(),
            STAGING_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        ensure_private_runtime_directory(&staging)?;
        let mut staging_guard = StagingGuard::new(staging.clone());
        write_managed_package_manifest(&staging, descriptor)?;
        self.installer.install(&staging, descriptor).await?;
        ensure_private_runtime_file(&staging.join("package.json"))?;
        ensure_private_runtime_file(&staging.join("package-lock.json"))?;
        let _verified_staging = self.inspect_at(descriptor, &staging)?;

        if target.exists() {
            return self.inspect_at(descriptor, &target);
        }
        if let Err(error) = fs::rename(&staging, &target) {
            // A concurrent installer may have published the same exact
            // version after our existence check. Accept only a fully verified
            // winner; every other rename failure remains fatal.
            if target.exists() {
                return self.inspect_at(descriptor, &target);
            }
            return Err(managed_storage_error(
                "acp_managed_install_publish_failed",
                "Verified ACP adapter staging directory could not be published",
                error,
            ));
        }
        staging_guard.disarm();
        self.inspect_at(descriptor, &target)
    }

    /// Side-effect-free inspection of an already installed managed adapter.
    pub fn inspect(
        &self,
        descriptor: &AcpAgentCompatibility,
    ) -> VibexResult<VerifiedAcpAdapterInstallation> {
        self.inspect_at(descriptor, &self.installation_root(descriptor)?)
    }

    fn inspect_at(
        &self,
        descriptor: &AcpAgentCompatibility,
        install_root: &Path,
    ) -> VibexResult<VerifiedAcpAdapterInstallation> {
        if !install_root.is_dir() {
            return Err(VibexError::process(
                "acp_managed_adapter_missing",
                "Managed ACP adapter is not installed",
            )
            .with_diagnostic("adapterId", descriptor.adapter_id.to_string())
            .with_diagnostic(
                "adapterVersion",
                descriptor.distribution.exact_version.to_string(),
            ));
        }
        let lock = read_package_lock(install_root)?;
        verify_locked_package(
            &lock,
            &descriptor.distribution.package,
            &descriptor.distribution.exact_version,
            &descriptor.distribution.integrity,
            &descriptor.distribution.registry_origin,
        )?;

        let adapter_dir = package_directory(install_root, &descriptor.distribution.package)?;
        verify_path_contained(install_root, &adapter_dir, "package")?;
        let adapter_manifest = read_package_manifest(&adapter_dir)?;
        verify_package_manifest(
            &adapter_manifest,
            &descriptor.distribution.package,
            &descriptor.distribution.exact_version,
        )?;
        let mut observed_node_requirement = (descriptor.distribution.node_requirement_package
            == descriptor.distribution.package)
            .then(|| adapter_manifest.engines.node.clone())
            .flatten();
        let bin_relative = adapter_manifest.bin_path(&descriptor.distribution.bin_name)?;
        validate_relative_package_path(&bin_relative)?;
        let entrypoint = adapter_dir.join(bin_relative);
        if !entrypoint.is_file() {
            return Err(VibexError::process(
                "acp_managed_adapter_bin_missing",
                "Managed ACP adapter bin entry does not exist",
            )
            .with_diagnostic("adapterId", descriptor.adapter_id.to_string())
            .with_diagnostic("bin", descriptor.distribution.bin_name.clone()));
        }
        verify_path_contained(&adapter_dir, &entrypoint, "bin")?;

        let mut runtime_versions = BTreeMap::new();
        for dependency in &descriptor.distribution.runtime_dependencies {
            verify_declared_runtime_dependency(&adapter_manifest, dependency)?;
            verify_locked_runtime_dependency(
                &lock,
                dependency,
                &descriptor.distribution.registry_origin,
            )?;
            let dependency_dir = package_directory(install_root, &dependency.package)?;
            verify_path_contained(install_root, &dependency_dir, "runtime dependency")?;
            let dependency_manifest = read_package_manifest(&dependency_dir)?;
            verify_package_manifest(
                &dependency_manifest,
                &dependency.package,
                &dependency.managed_pin,
            )?;
            if descriptor.distribution.node_requirement_package == dependency.package {
                observed_node_requirement = dependency_manifest.engines.node.clone();
            }
            if !dependency.override_declared_requirement
                && !dependency
                    .declared_requirement
                    .matches(&dependency_manifest.version)
            {
                return Err(VibexError::validation(
                    "acp_managed_dependency_requirement_mismatch",
                    "Managed ACP runtime dependency does not satisfy the adapter requirement",
                )
                .with_diagnostic("package", dependency.package.clone())
                .with_diagnostic("version", dependency_manifest.version.to_string()));
            }
            if dependency.include_in_compatibility_identity {
                runtime_versions.insert(
                    dependency.package.clone(),
                    dependency_manifest.version.clone(),
                );
            }
        }
        let observed_node_requirement = observed_node_requirement.ok_or_else(|| {
            VibexError::validation(
                "acp_managed_node_requirement_missing",
                "Managed ACP package metadata has no declared Node requirement",
            )
            .with_diagnostic(
                "package",
                descriptor.distribution.node_requirement_package.clone(),
            )
        })?;
        if observed_node_requirement != descriptor.distribution.node_requirement {
            return Err(VibexError::validation(
                "acp_managed_node_requirement_mismatch",
                "Managed ACP package Node requirement does not match the registry descriptor",
            )
            .with_diagnostic(
                "package",
                descriptor.distribution.node_requirement_package.clone(),
            )
            .with_diagnostic("actualRequirement", observed_node_requirement.to_string())
            .with_diagnostic(
                "expectedRequirement",
                descriptor.distribution.node_requirement.to_string(),
            ));
        }

        let binary_identity = hash_file(&entrypoint)?;
        Ok(VerifiedAcpAdapterInstallation {
            adapter_id: descriptor.adapter_id.clone(),
            adapter_version: adapter_manifest.version,
            compatibility_identity: AdapterCompatibilityIdentity::new(
                &descriptor.adapter_id,
                &descriptor.distribution.exact_version,
                &runtime_versions,
            ),
            binary_identity,
            runtime_versions,
            install_root: install_root.to_path_buf(),
            command: ManagedAdapterCommand {
                program: self.node_command.clone(),
                args: vec![entrypoint.to_string_lossy().into_owned()],
                current_dir: install_root.to_path_buf(),
            },
        })
    }

    pub async fn health_probe(
        &self,
        descriptor: &AcpAgentCompatibility,
    ) -> VibexResult<AcpAdapterHealthReport> {
        let installation = self.inspect(descriptor)?;
        let node_output = run_bounded_output(
            &installation.command.program,
            &["--version".to_string()],
            &installation.command.current_dir,
            self.probe_timeout,
        )
        .await?;
        let node_version = parse_version_output(&node_output).ok_or_else(|| {
            VibexError::validation(
                "acp_managed_node_version_invalid",
                "Managed ACP adapter Node version output could not be parsed",
            )
        })?;
        if !descriptor
            .distribution
            .node_requirement
            .matches(&node_version)
        {
            return Err(VibexError::process(
                "acp_managed_node_version_unsupported",
                "Managed ACP adapter requires a different Node version",
            )
            .with_diagnostic("nodeVersion", node_version.to_string())
            .with_diagnostic(
                "requiredVersion",
                descriptor.distribution.node_requirement.to_string(),
            ));
        }

        let command_variant = descriptor.command_variants.first().ok_or_else(|| {
            VibexError::validation(
                "acp_registry_command_missing",
                "ACP compatibility descriptor does not have a probe command",
            )
        })?;
        let reported_adapter_version = if command_variant.version_args.is_empty() {
            // Some stdio-only adapters start their protocol loop for every CLI
            // invocation and intentionally expose no version flag. Their exact
            // package version and integrity were already verified above.
            installation.adapter_version.clone()
        } else {
            let mut version_args = installation.command.args.clone();
            version_args.extend(command_variant.version_args.clone());
            let adapter_output = run_bounded_output(
                &installation.command.program,
                &version_args,
                &installation.command.current_dir,
                self.probe_timeout,
            )
            .await?;
            parse_version_output(&adapter_output).ok_or_else(|| {
                VibexError::validation(
                    "acp_managed_adapter_version_output_invalid",
                    "Managed ACP adapter version output could not be parsed",
                )
            })?
        };
        if reported_adapter_version != descriptor.distribution.exact_version {
            return Err(VibexError::process(
                "acp_managed_adapter_version_mismatch",
                "Managed ACP adapter reported a different version",
            )
            .with_diagnostic("adapterId", descriptor.adapter_id.to_string())
            .with_diagnostic("reportedVersion", reported_adapter_version.to_string())
            .with_diagnostic(
                "expectedVersion",
                descriptor.distribution.exact_version.to_string(),
            ));
        }

        let initialize = probe_initialize(
            &installation.command,
            &command_variant.args,
            self.probe_timeout,
        )
        .await?;
        let protocol_version = initialize.protocol_version.as_deref().ok_or_else(|| {
            VibexError::provider(
                "acp_initialize_protocol_version_missing",
                "ACP initialize did not report a protocol version",
            )
        })?;
        if protocol_version != "1" {
            return Err(VibexError::provider(
                "acp_initialize_protocol_version_mismatch",
                "ACP initialize reported an unsupported protocol version",
            )
            .with_diagnostic("reportedVersion", protocol_version));
        }
        let agent_name = initialize
            .agent_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| {
                VibexError::provider(
                    "acp_initialize_agent_name_missing",
                    "ACP initialize did not report an agent name",
                )
            })?;
        if agent_name != descriptor.distribution.initialize_agent_name {
            return Err(VibexError::provider(
                "acp_initialize_agent_name_mismatch",
                "ACP initialize reported a different agent name",
            )
            .with_diagnostic("adapterId", descriptor.adapter_id.to_string())
            .with_diagnostic("reportedName", agent_name));
        }
        let raw_agent_version = initialize.agent_version.as_deref().ok_or_else(|| {
            VibexError::provider(
                "acp_initialize_agent_version_missing",
                "ACP initialize did not report an agent version",
            )
        })?;
        let agent_version = parse_version_output(raw_agent_version).ok_or_else(|| {
            VibexError::provider(
                "acp_initialize_agent_version_invalid",
                "ACP initialize reported an invalid agent version",
            )
        })?;
        if agent_version != descriptor.distribution.exact_version {
            return Err(VibexError::provider(
                "acp_initialize_agent_version_mismatch",
                "ACP initialize reported a different adapter version",
            )
            .with_diagnostic("adapterId", descriptor.adapter_id.to_string())
            .with_diagnostic("reportedVersion", agent_version.to_string())
            .with_diagnostic(
                "expectedVersion",
                descriptor.distribution.exact_version.to_string(),
            ));
        }

        Ok(AcpAdapterHealthReport {
            adapter_id: descriptor.adapter_id.to_string(),
            adapter_version: installation.adapter_version.to_string(),
            compatibility_identity: installation.compatibility_identity.to_string(),
            binary_identity: installation.binary_identity,
            node_version: node_version.to_string(),
            reported_adapter_version: reported_adapter_version.to_string(),
            protocol_version: initialize.protocol_version,
            agent_name: initialize.agent_name,
            agent_version: initialize.agent_version,
        })
    }
}

#[derive(Debug)]
struct InitializeProbeResult {
    protocol_version: Option<String>,
    agent_name: Option<String>,
    agent_version: Option<String>,
}

async fn probe_initialize(
    managed_command: &ManagedAdapterCommand,
    adapter_args: &[String],
    probe_timeout: Duration,
) -> VibexResult<InitializeProbeResult> {
    let mut command = Command::new(&managed_command.program);
    command
        .args(&managed_command.args)
        .args(adapter_args)
        .current_dir(&managed_command.current_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    sanitize_inherited_appimage_environment(command.as_std_mut());
    for key in PARENT_SESSION_ENV_KEYS {
        command.env_remove(key);
    }
    for (key, value) in PROBE_ENV {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|error| {
        VibexError::process(
            "acp_health_probe_spawn_failed",
            "Managed ACP adapter health probe could not be started",
        )
        .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
    })?;
    let Some(mut stdin) = child.stdin.take() else {
        terminate_child(&mut child).await;
        return Err(probe_stdio_unavailable());
    };
    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child).await;
        return Err(probe_stdio_unavailable());
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_child(&mut child).await;
        return Err(probe_stdio_unavailable());
    };
    let stderr_task = tokio::spawn(read_bounded_stream(stderr));

    let read_result = async {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": AcpOperation::Initialize.method(),
            "params": build_initialize_params(false, false, false, false, false),
        });
        let line = serde_json::to_vec(&request).map_err(|error| {
            VibexError::validation(
                "acp_health_probe_request_encode_failed",
                "Managed ACP adapter initialize probe could not be encoded",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        stdin.write_all(&line).await.map_err(|error| {
            VibexError::process(
                "acp_health_probe_write_failed",
                "Managed ACP adapter initialize probe could not be written",
            )
            .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
        })?;
        stdin.write_all(b"\n").await.map_err(|error| {
            VibexError::process(
                "acp_health_probe_write_failed",
                "Managed ACP adapter initialize probe could not be terminated",
            )
            .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
        })?;
        stdin.flush().await.map_err(|error| {
            VibexError::process(
                "acp_health_probe_write_failed",
                "Managed ACP adapter initialize probe could not be flushed",
            )
            .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
        })?;

        timeout(probe_timeout, read_initialize_response(stdout))
            .await
            .map_err(|_| {
                VibexError::process(
                    "acp_health_probe_timeout",
                    "Managed ACP adapter initialize probe timed out",
                )
            })?
    }
    .await;

    terminate_child(&mut child).await;
    drop(stdin);
    let stderr = stderr_task.await.unwrap_or_default();
    read_result.map_err(|error| with_optional_stderr(error, &stderr))
}

async fn read_initialize_response<R>(stdout: R) -> VibexResult<InitializeProbeResult>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await.map_err(|error| {
        VibexError::process(
            "acp_health_probe_read_failed",
            "Managed ACP adapter initialize response could not be read",
        )
        .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
    })? {
        let message: Value = serde_json::from_str(&line).map_err(|error| {
            VibexError::provider(
                "acp_health_probe_response_invalid",
                "Managed ACP adapter emitted malformed initialize output",
            )
            .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
        })?;
        let _decoded = decode_incoming(&message);
        if message.get("id") != Some(&json!(1)) {
            continue;
        }
        if let Some(error) = message.get("error") {
            return Err(VibexError::provider(
                "acp_health_probe_initialize_failed",
                "Managed ACP adapter rejected initialize",
            )
            .with_diagnostic("error", bounded_diagnostic(&error.to_string())));
        }
        let result = message
            .get("result")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                VibexError::provider(
                    "acp_health_probe_initialize_shape_invalid",
                    "Managed ACP adapter initialize response has no result object",
                )
            })?;
        if !result
            .get("agentCapabilities")
            .is_some_and(Value::is_object)
        {
            return Err(VibexError::provider(
                "acp_health_probe_capabilities_shape_invalid",
                "Managed ACP adapter initialize response has invalid capabilities",
            ));
        }
        return Ok(InitializeProbeResult {
            protocol_version: result
                .get("protocolVersion")
                .and_then(value_as_version_string),
            agent_name: result
                .get("agentInfo")
                .and_then(|info| info.get("name"))
                .and_then(Value::as_str)
                .map(bounded_diagnostic),
            agent_version: result
                .get("agentInfo")
                .and_then(|info| info.get("version"))
                .and_then(Value::as_str)
                .map(bounded_diagnostic),
        });
    }
    Err(VibexError::process(
        "acp_health_probe_exited_before_initialize",
        "Managed ACP adapter exited before returning initialize",
    ))
}

fn probe_stdio_unavailable() -> VibexError {
    VibexError::process(
        "acp_health_probe_stdio_unavailable",
        "Managed ACP adapter health probe stdio is unavailable",
    )
}

async fn terminate_child(child: &mut tokio::process::Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

fn with_optional_stderr(error: VibexError, stderr: &str) -> VibexError {
    if stderr.trim().is_empty() {
        error
    } else {
        error.with_diagnostic("stderr", bounded_diagnostic(stderr))
    }
}

async fn read_bounded_stream<R>(reader: R) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    let mut output = String::new();
    while let Ok(Some(line)) = lines.next_line().await {
        if output.len() >= DIAGNOSTIC_LIMIT_BYTES {
            break;
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&line);
        if output.len() > DIAGNOSTIC_LIMIT_BYTES {
            output.truncate(DIAGNOSTIC_LIMIT_BYTES);
            output.push_str("...[truncated]");
            break;
        }
    }
    bounded_diagnostic(&output)
}

async fn run_bounded_output(
    program: &Path,
    args: &[String],
    current_dir: &Path,
    command_timeout: Duration,
) -> VibexResult<String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(current_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    sanitize_inherited_appimage_environment(command.as_std_mut());
    for key in PARENT_SESSION_ENV_KEYS {
        command.env_remove(key);
    }
    let output = timeout(command_timeout, command.output())
        .await
        .map_err(|_| {
            VibexError::process(
                "acp_health_probe_command_timeout",
                "Managed ACP adapter health command timed out",
            )
        })?
        .map_err(|error| {
            VibexError::process(
                "acp_health_probe_command_failed",
                "Managed ACP adapter health command could not be started",
            )
            .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
        })?;
    if !output.status.success() {
        return Err(VibexError::process(
            "acp_health_probe_command_failed",
            "Managed ACP adapter health command failed",
        )
        .with_diagnostic(
            "exitCode",
            output
                .status
                .code()
                .map_or_else(|| "signal".to_string(), |code| code.to_string()),
        )
        .with_diagnostic(
            "stderr",
            bounded_diagnostic(&String::from_utf8_lossy(&output.stderr)),
        ));
    }
    Ok(bounded_diagnostic(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_version_output(output: &str) -> Option<Version> {
    output.split_whitespace().find_map(|token| {
        let token = token.trim_matches(|character: char| {
            !character.is_ascii_alphanumeric()
                && character != '.'
                && character != '-'
                && character != '+'
        });
        let token = token.strip_prefix('v').unwrap_or(token);
        Version::parse(token).ok()
    })
}

fn value_as_version_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(bounded_diagnostic(value)),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct PackageManifest {
    name: String,
    version: Version,
    #[serde(default)]
    bin: NpmBin,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default)]
    engines: PackageEngines,
}

impl PackageManifest {
    fn bin_path(&self, bin_name: &str) -> VibexResult<PathBuf> {
        let value = match &self.bin {
            NpmBin::Single(path) => path,
            NpmBin::Map(entries) => entries.get(bin_name).ok_or_else(|| {
                VibexError::validation(
                    "acp_managed_adapter_bin_mismatch",
                    "Managed ACP adapter package does not declare the registry bin",
                )
                .with_diagnostic("package", self.name.clone())
                .with_diagnostic("bin", bin_name)
            })?,
            NpmBin::Missing => {
                return Err(VibexError::validation(
                    "acp_managed_adapter_bin_mismatch",
                    "Managed ACP adapter package has no bin entry",
                )
                .with_diagnostic("package", self.name.clone()));
            }
        };
        Ok(PathBuf::from(value))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum NpmBin {
    Single(String),
    Map(BTreeMap<String, String>),
    #[default]
    Missing,
}

#[derive(Debug, Default, Deserialize)]
struct PackageEngines {
    node: Option<semver::VersionReq>,
}

#[derive(Debug, Deserialize)]
struct PackageLock {
    #[serde(default)]
    packages: BTreeMap<String, LockedPackage>,
}

#[derive(Debug, Deserialize)]
struct LockedPackage {
    version: Option<Version>,
    integrity: Option<String>,
    resolved: Option<String>,
}

fn read_package_manifest(package_dir: &Path) -> VibexResult<PackageManifest> {
    let path = package_dir.join("package.json");
    let bytes = fs::read(&path).map_err(|error| {
        managed_storage_error(
            "acp_managed_package_manifest_read_failed",
            "Managed ACP package manifest could not be read",
            error,
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        VibexError::validation(
            "acp_managed_package_manifest_invalid",
            "Managed ACP package manifest is invalid",
        )
        .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
    })
}

fn read_package_lock(install_root: &Path) -> VibexResult<PackageLock> {
    let bytes = fs::read(install_root.join("package-lock.json")).map_err(|error| {
        managed_storage_error(
            "acp_managed_package_lock_read_failed",
            "Managed ACP package lock could not be read",
            error,
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        VibexError::validation(
            "acp_managed_package_lock_invalid",
            "Managed ACP package lock is invalid",
        )
        .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
    })
}

fn verify_package_manifest(
    manifest: &PackageManifest,
    package: &str,
    version: &Version,
) -> VibexResult<()> {
    if manifest.name != package || &manifest.version != version {
        return Err(VibexError::validation(
            "acp_managed_package_version_mismatch",
            "Managed ACP package manifest does not match the registry descriptor",
        )
        .with_diagnostic("package", package)
        .with_diagnostic("expectedVersion", version.to_string())
        .with_diagnostic("actualVersion", manifest.version.to_string()));
    }
    Ok(())
}

fn verify_declared_runtime_dependency(
    adapter_manifest: &PackageManifest,
    dependency: &ManagedRuntimeDependency,
) -> VibexResult<()> {
    let declared = adapter_manifest
        .dependencies
        .get(&dependency.package)
        .ok_or_else(|| {
            VibexError::validation(
                "acp_managed_dependency_declaration_missing",
                "Managed ACP adapter does not declare the required runtime dependency",
            )
            .with_diagnostic("package", dependency.package.clone())
        })?;
    let declared = semver::VersionReq::parse(declared).map_err(|error| {
        VibexError::validation(
            "acp_managed_dependency_declaration_invalid",
            "Managed ACP adapter declares an invalid runtime dependency requirement",
        )
        .with_diagnostic("package", dependency.package.clone())
        .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
    })?;
    if declared != dependency.declared_requirement {
        return Err(VibexError::validation(
            "acp_managed_dependency_declaration_mismatch",
            "Managed ACP adapter runtime dependency requirement does not match the registry",
        )
        .with_diagnostic("package", dependency.package.clone())
        .with_diagnostic("actualRequirement", declared.to_string())
        .with_diagnostic(
            "expectedRequirement",
            dependency.declared_requirement.to_string(),
        ));
    }
    Ok(())
}

fn verify_locked_package(
    lock: &PackageLock,
    package: &str,
    version: &Version,
    integrity: &str,
    registry_origin: &str,
) -> VibexResult<()> {
    let key = format!("node_modules/{package}");
    let locked = lock.packages.get(&key).ok_or_else(|| {
        VibexError::validation(
            "acp_managed_package_lock_entry_missing",
            "Managed ACP package lock has no entry for a required package",
        )
        .with_diagnostic("package", package)
    })?;
    verify_locked_package_entry(locked, package, version, integrity, registry_origin)
}

fn verify_locked_runtime_dependency(
    lock: &PackageLock,
    dependency: &ManagedRuntimeDependency,
    registry_origin: &str,
) -> VibexResult<()> {
    let package_suffix = format!("node_modules/{}", dependency.package);
    verify_locked_package(
        lock,
        &dependency.package,
        &dependency.managed_pin,
        &dependency.integrity,
        registry_origin,
    )?;
    let nested_entries = lock
        .packages
        .iter()
        .filter(|(path, _)| {
            path.as_str() != package_suffix
                && path
                    .strip_suffix(&package_suffix)
                    .is_some_and(|prefix| prefix.ends_with('/'))
        })
        .collect::<Vec<_>>();
    for (_, locked) in nested_entries {
        verify_locked_package_entry(
            locked,
            &dependency.package,
            &dependency.managed_pin,
            &dependency.integrity,
            registry_origin,
        )?;
    }
    Ok(())
}

fn verify_locked_package_entry(
    locked: &LockedPackage,
    package: &str,
    version: &Version,
    integrity: &str,
    registry_origin: &str,
) -> VibexResult<()> {
    if locked.version.as_ref() != Some(version) {
        return Err(VibexError::validation(
            "acp_managed_package_lock_version_mismatch",
            "Managed ACP package lock contains a different package version",
        )
        .with_diagnostic("package", package)
        .with_diagnostic("expectedVersion", version.to_string())
        .with_diagnostic(
            "actualVersion",
            locked
                .version
                .as_ref()
                .map_or_else(|| "missing".to_string(), ToString::to_string),
        ));
    }
    if locked.integrity.as_deref() != Some(integrity) {
        return Err(VibexError::validation(
            "acp_managed_package_integrity_mismatch",
            "Managed ACP package lock integrity does not match the registry descriptor",
        )
        .with_diagnostic("package", package));
    }
    let resolved = locked.resolved.as_deref().ok_or_else(|| {
        VibexError::validation(
            "acp_managed_package_resolved_missing",
            "Managed ACP package lock has no resolved source URL",
        )
        .with_diagnostic("package", package)
    })?;
    if !resolved_url_matches_registry(resolved, registry_origin, package, version) {
        return Err(VibexError::validation(
            "acp_managed_package_source_mismatch",
            "Managed ACP package resolved source does not match the trusted registry",
        )
        .with_diagnostic("package", package));
    }
    Ok(())
}

fn resolved_url_matches_registry(
    resolved: &str,
    registry_origin: &str,
    package: &str,
    version: &Version,
) -> bool {
    let artifact = package.rsplit('/').next().unwrap_or(package);
    resolved == format!("{registry_origin}/{package}/-/{artifact}-{version}.tgz")
}

fn package_directory(install_root: &Path, package: &str) -> VibexResult<PathBuf> {
    let mut path = install_root.join("node_modules");
    for segment in package.split('/') {
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.contains(['\\', ':', '\0'])
        {
            return Err(VibexError::validation(
                "acp_registry_package_name_invalid",
                "ACP registry package name is not safe for a managed install path",
            ));
        }
        path.push(segment);
    }
    Ok(path)
}

fn verify_path_contained(root: &Path, candidate: &Path, kind: &str) -> VibexResult<()> {
    let canonical_root = root.canonicalize().map_err(|error| {
        managed_storage_error(
            "acp_managed_path_resolve_failed",
            "Managed ACP root could not be resolved",
            error,
        )
    })?;
    let canonical_candidate = candidate.canonicalize().map_err(|error| {
        managed_storage_error(
            "acp_managed_path_resolve_failed",
            "Managed ACP path could not be resolved",
            error,
        )
    })?;
    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(VibexError::validation(
            "acp_managed_path_escape",
            "Managed ACP package path escapes its verified root",
        )
        .with_diagnostic("pathKind", kind));
    }
    Ok(())
}

fn validate_relative_package_path(path: &Path) -> VibexResult<()> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(VibexError::validation(
            "acp_managed_adapter_bin_path_invalid",
            "Managed ACP adapter bin path must stay inside its package",
        ));
    }
    Ok(())
}

fn hash_file(path: &Path) -> VibexResult<String> {
    let bytes = fs::read(path).map_err(|error| {
        managed_storage_error(
            "acp_managed_adapter_bin_read_failed",
            "Managed ACP adapter bin could not be read",
            error,
        )
    })?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{digest:x}"))
}

fn write_managed_package_manifest(
    install_root: &Path,
    descriptor: &AcpAgentCompatibility,
) -> VibexResult<()> {
    let mut dependencies = serde_json::Map::new();
    dependencies.insert(
        descriptor.distribution.package.clone(),
        Value::String(descriptor.distribution.exact_version.to_string()),
    );
    for dependency in &descriptor.distribution.runtime_dependencies {
        dependencies.insert(
            dependency.package.clone(),
            Value::String(dependency.managed_pin.to_string()),
        );
    }
    let runtime_overrides = descriptor
        .distribution
        .runtime_dependencies
        .iter()
        .filter(|dependency| dependency.override_declared_requirement)
        .map(|dependency| {
            (
                dependency.package.clone(),
                Value::String(dependency.managed_pin.to_string()),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let mut overrides = serde_json::Map::new();
    if !runtime_overrides.is_empty() {
        overrides.insert(
            descriptor.distribution.package.clone(),
            Value::Object(runtime_overrides),
        );
    }
    let manifest = json!({
        "name": format!("vibex-managed-{}", descriptor.adapter_id),
        "version": "0.0.0",
        "private": true,
        "dependencies": dependencies,
        "overrides": overrides,
    });
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| {
        VibexError::validation(
            "acp_managed_install_manifest_encode_failed",
            "Managed ACP install manifest could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    write_private_runtime_file_atomic(&install_root.join("package.json"), &bytes).map_err(|error| {
        VibexError::storage(
            "acp_managed_install_manifest_write_failed",
            "Managed ACP install manifest could not be written",
        )
        .with_diagnostic("causeCode", error.code)
    })
}

fn validate_managed_root(root: &Path) -> VibexResult<()> {
    if !root.is_absolute() {
        return Err(VibexError::validation(
            "acp_managed_install_root_relative",
            "Managed ACP adapter root must be an absolute path",
        ));
    }
    Ok(())
}

fn managed_storage_error(code: &str, message: &str, error: impl fmt::Display) -> VibexError {
    VibexError::storage(code, message)
        .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
}

fn bounded_diagnostic(value: &str) -> String {
    let home = std::env::var("HOME").ok();
    let mut value = home
        .as_deref()
        .filter(|home| !home.is_empty())
        .map_or_else(|| value.to_string(), |home| value.replace(home, "[home]"));
    for marker in ["authorization", "api_key", "apikey", "token", "secret"] {
        if value.to_ascii_lowercase().contains(marker) {
            return "[redacted-sensitive-diagnostic]".to_string();
        }
    }
    if value.len() > DIAGNOSTIC_LIMIT_BYTES {
        value.truncate(DIAGNOSTIC_LIMIT_BYTES);
        value.push_str("...[truncated]");
    }
    value
}

struct StagingGuard {
    path: Option<PathBuf>,
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[async_trait]
trait NpmInstallExecutor: Send + Sync {
    async fn install(
        &self,
        install_root: &Path,
        descriptor: &AcpAgentCompatibility,
    ) -> VibexResult<()>;
}

struct SystemNpmInstallExecutor {
    npm_command: PathBuf,
    timeout: Duration,
}

#[async_trait]
impl NpmInstallExecutor for SystemNpmInstallExecutor {
    async fn install(
        &self,
        install_root: &Path,
        descriptor: &AcpAgentCompatibility,
    ) -> VibexResult<()> {
        let mut command = Command::new(&self.npm_command);
        command
            .arg("install")
            .arg("--registry")
            .arg(&descriptor.distribution.registry_origin)
            .arg("--ignore-scripts")
            .arg("--no-audit")
            .arg("--no-fund")
            .arg("--omit=dev")
            .arg("--package-lock=true")
            .arg("--prefix")
            .arg(install_root)
            .current_dir(install_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        sanitize_inherited_appimage_environment(command.as_std_mut());
        let output = timeout(self.timeout, command.output())
            .await
            .map_err(|_| {
                VibexError::process(
                    "acp_managed_install_timeout",
                    "Managed ACP adapter npm install timed out",
                )
                .with_diagnostic("adapterId", descriptor.adapter_id.to_string())
            })?
            .map_err(|error| {
                VibexError::process(
                    "acp_managed_install_spawn_failed",
                    "Managed ACP adapter npm install could not be started",
                )
                .with_diagnostic("adapterId", descriptor.adapter_id.to_string())
                .with_diagnostic("error", bounded_diagnostic(&error.to_string()))
            })?;
        if !output.status.success() {
            return Err(VibexError::process(
                "acp_managed_install_failed",
                "Managed ACP adapter npm install failed",
            )
            .with_diagnostic("adapterId", descriptor.adapter_id.to_string())
            .with_diagnostic(
                "exitCode",
                output
                    .status
                    .code()
                    .map_or_else(|| "signal".to_string(), |code| code.to_string()),
            )
            .with_diagnostic(
                "stderr",
                bounded_diagnostic(&String::from_utf8_lossy(&output.stderr)),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{
        AcpCompatibilityRegistry, CLAUDE_ADAPTER_VERSION, CLAUDE_AGENT_ID, CODEX_ADAPTER_INTEGRITY,
        CODEX_ADAPTER_VERSION, CODEX_AGENT_ID, CODEX_RUNTIME_DECLARED_REQUIREMENT,
        CODEX_RUNTIME_INTEGRITY, CODEX_RUNTIME_PACKAGE, CODEX_RUNTIME_PIN,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use tokio::sync::Barrier;
    use vibex_core::AgentId;

    fn descriptor(agent_id: &str) -> AcpAgentCompatibility {
        AcpCompatibilityRegistry::builtin()
            .unwrap()
            .for_agent(&AgentId::parse(agent_id).unwrap())
            .unwrap()
            .clone()
    }

    struct FixtureInstaller {
        calls: AtomicUsize,
        script: String,
    }

    struct RacingInstaller {
        barrier: Arc<Barrier>,
        script: String,
    }

    #[async_trait]
    impl NpmInstallExecutor for RacingInstaller {
        async fn install(
            &self,
            install_root: &Path,
            descriptor: &AcpAgentCompatibility,
        ) -> VibexResult<()> {
            write_fixture_install(install_root, descriptor, &self.script);
            self.barrier.wait().await;
            Ok(())
        }
    }

    #[async_trait]
    impl NpmInstallExecutor for FixtureInstaller {
        async fn install(
            &self,
            install_root: &Path,
            descriptor: &AcpAgentCompatibility,
        ) -> VibexResult<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            write_fixture_install(install_root, descriptor, &self.script);
            Ok(())
        }
    }

    fn fixture_store(
        temp: &TempDir,
        script: &str,
        probe_timeout: Duration,
    ) -> (ManagedAcpAdapterStore, Arc<FixtureInstaller>) {
        let installer = Arc::new(FixtureInstaller {
            calls: AtomicUsize::new(0),
            script: script.to_string(),
        });
        let store = ManagedAcpAdapterStore::with_installer(
            temp.path().join("managed"),
            "node",
            installer.clone(),
            probe_timeout,
        )
        .unwrap();
        (store, installer)
    }

    fn write_fixture_install(
        install_root: &Path,
        descriptor: &AcpAgentCompatibility,
        script: &str,
    ) {
        let adapter_dir =
            package_directory(install_root, &descriptor.distribution.package).unwrap();
        fs::create_dir_all(adapter_dir.join("dist")).unwrap();
        let declared_dependencies: serde_json::Map<String, Value> = descriptor
            .distribution
            .runtime_dependencies
            .iter()
            .map(|dependency| {
                (
                    dependency.package.clone(),
                    Value::String(dependency.declared_requirement.to_string()),
                )
            })
            .collect();
        let adapter_engines = if descriptor.distribution.node_requirement_package
            == descriptor.distribution.package
        {
            json!({ "node": descriptor.distribution.node_requirement.to_string() })
        } else {
            json!({})
        };
        fs::write(
            adapter_dir.join("package.json"),
            serde_json::to_vec(&json!({
                "name": descriptor.distribution.package,
                "version": descriptor.distribution.exact_version,
                "bin": { descriptor.distribution.bin_name.clone(): "dist/index.js" },
                "dependencies": declared_dependencies,
                "engines": adapter_engines,
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(adapter_dir.join("dist/index.js"), script).unwrap();

        let mut packages = serde_json::Map::new();
        packages.insert(
            format!("node_modules/{}", descriptor.distribution.package),
            json!({
                "version": descriptor.distribution.exact_version,
                "integrity": descriptor.distribution.integrity,
                "resolved": fixture_resolved_url(
                    &descriptor.distribution.registry_origin,
                    &descriptor.distribution.package,
                    &descriptor.distribution.exact_version,
                ),
            }),
        );
        for dependency in &descriptor.distribution.runtime_dependencies {
            let dependency_dir = package_directory(install_root, &dependency.package).unwrap();
            fs::create_dir_all(&dependency_dir).unwrap();
            fs::write(
                dependency_dir.join("package.json"),
                serde_json::to_vec(&json!({
                    "name": dependency.package,
                    "version": dependency.managed_pin,
                    "engines": if descriptor.distribution.node_requirement_package
                        == dependency.package
                    {
                        json!({ "node": descriptor.distribution.node_requirement.to_string() })
                    } else {
                        json!({})
                    },
                }))
                .unwrap(),
            )
            .unwrap();
            packages.insert(
                format!("node_modules/{}", dependency.package),
                json!({
                    "version": dependency.managed_pin,
                    "integrity": dependency.integrity,
                    "resolved": fixture_resolved_url(
                        &descriptor.distribution.registry_origin,
                        &dependency.package,
                        &dependency.managed_pin,
                    ),
                }),
            );
        }
        fs::write(
            install_root.join("package-lock.json"),
            serde_json::to_vec(&json!({ "lockfileVersion": 3, "packages": packages })).unwrap(),
        )
        .unwrap();
    }

    fn fixture_resolved_url(registry_origin: &str, package: &str, version: &Version) -> String {
        let artifact = package.rsplit('/').next().unwrap_or(package);
        format!("{registry_origin}/{package}/-/{artifact}-{version}.tgz")
    }

    const HEALTHY_SCRIPT: &str = r#"
import readline from 'node:readline';
if (process.argv.includes('--version')) {
  console.log('0.64.2');
  process.exit(0);
}
const rl = readline.createInterface({ input: process.stdin });
rl.once('line', (line) => {
  const request = JSON.parse(line);
  console.log(JSON.stringify({
    jsonrpc: '2.0', id: request.id,
    result: {
      protocolVersion: 1,
      agentInfo: { name: '@agentclientprotocol/claude-agent-acp', version: '0.64.2' },
      agentCapabilities: {}
    }
  }));
});
"#;

    #[tokio::test]
    async fn isolated_install_is_atomic_verified_and_idempotent() {
        let temp = TempDir::new().unwrap();
        let (store, installer) = fixture_store(&temp, HEALTHY_SCRIPT, Duration::from_secs(2));
        let descriptor = descriptor(CODEX_AGENT_ID);
        let installed = store.install(&descriptor).await.unwrap();
        assert_eq!(installer.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            installed.runtime_versions.get("@openai/codex"),
            Some(&Version::parse(CODEX_RUNTIME_PIN).unwrap())
        );
        assert_eq!(
            installed.compatibility_identity.as_str(),
            descriptor.expected_compatibility_identity().as_str()
        );
        let managed_manifest: Value =
            serde_json::from_slice(&fs::read(installed.install_root.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(
            managed_manifest["overrides"][descriptor.distribution.package.as_str()]
                [CODEX_RUNTIME_PACKAGE],
            CODEX_RUNTIME_PIN
        );
        assert!(installed.binary_identity.starts_with("sha256:"));
        assert!(installed.install_root.is_dir());
        assert!(
            fs::read_dir(installed.install_root.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".staging-"))
        );

        let second = store.install(&descriptor).await.unwrap();
        assert_eq!(second, installed);
        assert_eq!(installer.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn concurrent_exact_installers_accept_the_verified_publish_winner() {
        let temp = TempDir::new().unwrap();
        let installer = Arc::new(RacingInstaller {
            barrier: Arc::new(Barrier::new(2)),
            script: HEALTHY_SCRIPT.to_string(),
        });
        let store = ManagedAcpAdapterStore::with_installer(
            temp.path().join("managed"),
            "node",
            installer,
            Duration::from_secs(2),
        )
        .unwrap();
        let descriptor = descriptor(CODEX_AGENT_ID);
        let (first, second) = tokio::join!(store.install(&descriptor), store.install(&descriptor));
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first, second);
        assert!(first.install_root.is_dir());
    }

    #[test]
    fn inspection_rejects_integrity_and_runtime_version_mismatches() {
        let temp = TempDir::new().unwrap();
        let (store, _) = fixture_store(&temp, HEALTHY_SCRIPT, Duration::from_secs(2));
        let descriptor = descriptor(CODEX_AGENT_ID);
        let root = store.installation_root(&descriptor).unwrap();
        fs::create_dir_all(&root).unwrap();
        write_fixture_install(&root, &descriptor, HEALTHY_SCRIPT);

        let lock_path = root.join("package-lock.json");
        let mut lock: Value = serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
        lock["packages"][format!("node_modules/{}", descriptor.distribution.package)]["integrity"] =
            json!("sha512-wrong");
        fs::write(&lock_path, serde_json::to_vec(&lock).unwrap()).unwrap();
        let err = store.inspect(&descriptor).unwrap_err();
        assert_eq!(err.code, "acp_managed_package_integrity_mismatch");

        lock["packages"][format!("node_modules/{}", descriptor.distribution.package)]["integrity"] =
            json!(CODEX_ADAPTER_INTEGRITY);
        lock["packages"][format!("node_modules/{}", descriptor.distribution.package)]["resolved"] =
            json!("https://evil.example.test/codex-acp.tgz");
        fs::write(&lock_path, serde_json::to_vec(&lock).unwrap()).unwrap();
        let err = store.inspect(&descriptor).unwrap_err();
        assert_eq!(err.code, "acp_managed_package_source_mismatch");

        lock["packages"][format!("node_modules/{}", descriptor.distribution.package)]["resolved"] =
            json!(fixture_resolved_url(
                &descriptor.distribution.registry_origin,
                &descriptor.distribution.package,
                &descriptor.distribution.exact_version,
            ));
        lock["packages"]["node_modules/@openai/codex"]["version"] = json!("0.145.0");
        lock["packages"]["node_modules/@openai/codex"]["integrity"] =
            json!(CODEX_RUNTIME_INTEGRITY);
        fs::write(&lock_path, serde_json::to_vec(&lock).unwrap()).unwrap();
        let err = store.inspect(&descriptor).unwrap_err();
        assert_eq!(err.code, "acp_managed_package_lock_version_mismatch");

        lock["packages"]["node_modules/@openai/codex"]["version"] = json!(CODEX_RUNTIME_PIN);
        lock["packages"]["node_modules/@agentclientprotocol/codex-acp/node_modules/@openai/codex"] = json!({
            "version": "0.145.0",
            "integrity": CODEX_RUNTIME_INTEGRITY,
            "resolved": fixture_resolved_url(
                &descriptor.distribution.registry_origin,
                CODEX_RUNTIME_PACKAGE,
                &Version::parse("0.145.0").unwrap(),
            ),
        });
        fs::write(&lock_path, serde_json::to_vec(&lock).unwrap()).unwrap();
        let err = store.inspect(&descriptor).unwrap_err();
        assert_eq!(err.code, "acp_managed_package_lock_version_mismatch");

        lock["packages"]
            .as_object_mut()
            .unwrap()
            .remove("node_modules/@agentclientprotocol/codex-acp/node_modules/@openai/codex");
        fs::write(&lock_path, serde_json::to_vec(&lock).unwrap()).unwrap();
        let adapter_dir = package_directory(&root, &descriptor.distribution.package).unwrap();
        let adapter_manifest_path = adapter_dir.join("package.json");
        let mut adapter_manifest: Value =
            serde_json::from_slice(&fs::read(&adapter_manifest_path).unwrap()).unwrap();
        adapter_manifest["dependencies"]["@openai/codex"] = json!("^0.143.0");
        fs::write(
            &adapter_manifest_path,
            serde_json::to_vec(&adapter_manifest).unwrap(),
        )
        .unwrap();
        let err = store.inspect(&descriptor).unwrap_err();
        assert_eq!(err.code, "acp_managed_dependency_declaration_mismatch");

        adapter_manifest["dependencies"]["@openai/codex"] =
            json!(CODEX_RUNTIME_DECLARED_REQUIREMENT);
        fs::write(
            &adapter_manifest_path,
            serde_json::to_vec(&adapter_manifest).unwrap(),
        )
        .unwrap();
        let runtime_manifest_path = package_directory(&root, "@openai/codex")
            .unwrap()
            .join("package.json");
        let mut runtime_manifest: Value =
            serde_json::from_slice(&fs::read(&runtime_manifest_path).unwrap()).unwrap();
        runtime_manifest["engines"]["node"] = json!(">=18");
        fs::write(
            runtime_manifest_path,
            serde_json::to_vec(&runtime_manifest).unwrap(),
        )
        .unwrap();
        let err = store.inspect(&descriptor).unwrap_err();
        assert_eq!(err.code, "acp_managed_node_requirement_mismatch");
    }

    #[cfg(unix)]
    #[test]
    fn inspection_rejects_bin_symlinks_that_escape_the_package() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let (store, _) = fixture_store(&temp, HEALTHY_SCRIPT, Duration::from_secs(2));
        let descriptor = descriptor(CLAUDE_AGENT_ID);
        let root = store.installation_root(&descriptor).unwrap();
        fs::create_dir_all(&root).unwrap();
        write_fixture_install(&root, &descriptor, HEALTHY_SCRIPT);
        let adapter_dir = package_directory(&root, &descriptor.distribution.package).unwrap();
        let entrypoint = adapter_dir.join("dist/index.js");
        let outside = temp.path().join("outside.js");
        fs::write(&outside, HEALTHY_SCRIPT).unwrap();
        fs::remove_file(&entrypoint).unwrap();
        symlink(&outside, &entrypoint).unwrap();

        let err = store.inspect(&descriptor).unwrap_err();
        assert_eq!(err.code, "acp_managed_path_escape");
    }

    #[tokio::test]
    async fn health_probe_checks_node_adapter_version_and_initialize() {
        let temp = TempDir::new().unwrap();
        let (store, _) = fixture_store(&temp, HEALTHY_SCRIPT, Duration::from_secs(2));
        let descriptor = descriptor(CLAUDE_AGENT_ID);
        store.install(&descriptor).await.unwrap();
        let report = store.health_probe(&descriptor).await.unwrap();
        assert_eq!(report.adapter_version, CLAUDE_ADAPTER_VERSION);
        assert_eq!(report.reported_adapter_version, CLAUDE_ADAPTER_VERSION);
        assert_eq!(report.protocol_version.as_deref(), Some("1"));
        assert_eq!(
            report.agent_version.as_deref(),
            Some(CLAUDE_ADAPTER_VERSION)
        );
    }

    #[tokio::test]
    async fn health_probe_rejects_wrong_version_and_timeout() {
        let wrong_version_script = HEALTHY_SCRIPT.replace("0.64.2", "0.64.1");
        let temp = TempDir::new().unwrap();
        let (store, _) = fixture_store(&temp, &wrong_version_script, Duration::from_secs(2));
        let descriptor = descriptor(CLAUDE_AGENT_ID);
        store.install(&descriptor).await.unwrap();
        let err = store.health_probe(&descriptor).await.unwrap_err();
        assert_eq!(err.code, "acp_managed_adapter_version_mismatch");

        let hanging_script = r#"
if (process.argv.includes('--version')) { console.log('0.64.2'); process.exit(0); }
process.stdin.resume();
"#;
        let temp = TempDir::new().unwrap();
        let (store, _) = fixture_store(&temp, hanging_script, Duration::from_millis(100));
        store.install(&descriptor).await.unwrap();
        let err = store.health_probe(&descriptor).await.unwrap_err();
        assert_eq!(err.code, "acp_health_probe_timeout");
    }

    #[tokio::test]
    async fn health_probe_reports_early_exit_and_redacts_sensitive_stderr() {
        let script = r#"
if (process.argv.includes('--version')) { console.log('0.64.2'); process.exit(0); }
console.error('token=must-not-leak');
process.exit(7);
"#;
        let temp = TempDir::new().unwrap();
        let (store, _) = fixture_store(&temp, script, Duration::from_secs(2));
        let descriptor = descriptor(CLAUDE_AGENT_ID);
        store.install(&descriptor).await.unwrap();
        let err = store.health_probe(&descriptor).await.unwrap_err();
        assert_eq!(err.code, "acp_health_probe_exited_before_initialize");
        let serialized = serde_json::to_string(&err).unwrap();
        assert!(!serialized.contains("must-not-leak"));
        assert!(serialized.contains("redacted-sensitive-diagnostic"));
    }

    #[tokio::test]
    async fn health_probe_rejects_malformed_initialize_metadata() {
        let malformed_script = r#"
import readline from 'node:readline';
if (process.argv.includes('--version')) { console.log('0.64.2'); process.exit(0); }
const rl = readline.createInterface({ input: process.stdin });
rl.once('line', (line) => {
  const request = JSON.parse(line);
  console.log(JSON.stringify({
    jsonrpc: '2.0', id: request.id,
    result: {
      protocolVersion: 1,
      agentInfo: { name: '@agentclientprotocol/claude-agent-acp', version: '0.64.2' },
      agentCapabilities: []
    }
  }));
});
"#;
        let temp = TempDir::new().unwrap();
        let (store, _) = fixture_store(&temp, malformed_script, Duration::from_secs(2));
        let descriptor = descriptor(CLAUDE_AGENT_ID);
        store.install(&descriptor).await.unwrap();
        let err = store.health_probe(&descriptor).await.unwrap_err();
        assert_eq!(err.code, "acp_health_probe_capabilities_shape_invalid");
    }

    #[test]
    fn version_parser_handles_node_and_scoped_adapter_output() {
        assert_eq!(
            parse_version_output("v26.4.0"),
            Some(Version::parse("26.4.0").unwrap())
        );
        assert_eq!(
            parse_version_output(&format!(
                "@agentclientprotocol/codex-acp {CODEX_ADAPTER_VERSION}"
            )),
            Some(Version::parse(CODEX_ADAPTER_VERSION).unwrap())
        );
    }

    #[test]
    fn managed_root_must_be_absolute() {
        let err = ManagedAcpAdapterStore::new("relative").err().unwrap();
        assert_eq!(err.code, "acp_managed_install_root_relative");
    }

    #[tokio::test]
    async fn store_rejects_unregistered_path_escaping_adapter_ids() {
        let temp = TempDir::new().unwrap();
        let (store, installer) = fixture_store(&temp, HEALTHY_SCRIPT, Duration::from_secs(2));
        let mut descriptor = descriptor(CLAUDE_AGENT_ID);
        descriptor.adapter_id = AcpAdapterId::parse("../../outside").unwrap();

        let err = store.install(&descriptor).await.unwrap_err();
        assert_eq!(err.code, "acp_managed_adapter_id_path_invalid");
        assert_eq!(installer.calls.load(Ordering::Relaxed), 0);
        assert!(!temp.path().join("outside").exists());
    }
}

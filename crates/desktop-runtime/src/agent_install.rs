//! Verified, side-by-side ACP Agent installations owned by the desktop runtime.

use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use futures_util::StreamExt as _;
use reqwest::{Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::timeout;
use url::Url;
use vibex_config_switch::ProviderConfigService;
use vibex_core::{
    AgentCommandConfig, AgentId, AgentListRequest, AgentManagedDistributionKind,
    AgentManagedInstallState, AgentManagedInstallStatus, AgentUpdateConfigRequest, VibexError,
    VibexResult, acp_registry_agent_id, unix_timestamp_ms,
};
use vibex_db::{
    AgentManagedInstallationRecord, AgentManagedInstallationRepository, apply_migrations,
    open_database,
};

const ACP_REGISTRY_URL: &str =
    "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";
const REGISTRY_CACHE_MAX_AGE_MS: i64 = 60 * 60 * 1_000;
const REGISTRY_MAX_BYTES: usize = 5 * 1024 * 1024;
const DOWNLOAD_MAX_BYTES: u64 = 768 * 1024 * 1024;
const ARCHIVE_MAX_ENTRIES: usize = 100_000;
const ARCHIVE_MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const NODE_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const MINIMUM_NODE_VERSION: semver::Version = semver::Version::new(22, 0, 0);
const NODE_RELEASE_INDEX_URL: &str = "https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt";
const AGENT_NODE_PATH_ENV: &str = "VIBEX_AGENT_NODE_PATH";
const AGENT_NPM_PATH_ENV: &str = "VIBEX_AGENT_NPM_PATH";
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentNodeRuntimeOptions {
    pub node_path: Option<PathBuf>,
    pub npm_path: Option<PathBuf>,
}

impl AgentNodeRuntimeOptions {
    pub fn from_environment() -> Self {
        Self {
            node_path: nonempty_environment_path(AGENT_NODE_PATH_ENV),
            npm_path: nonempty_environment_path(AGENT_NPM_PATH_ENV),
        }
    }
}

#[derive(Clone)]
pub struct AgentInstallService {
    db_path: PathBuf,
    root: PathBuf,
    config_service: ProviderConfigService,
    node_runtime_options: AgentNodeRuntimeOptions,
    client: Client,
    operation_lock: Arc<Mutex<()>>,
}

impl AgentInstallService {
    pub fn new(
        db_path: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
        config_service: ProviderConfigService,
    ) -> VibexResult<Self> {
        Self::new_with_node_runtime_options(
            db_path,
            root,
            config_service,
            AgentNodeRuntimeOptions::default(),
        )
    }

    pub fn new_with_node_runtime_options(
        db_path: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
        config_service: ProviderConfigService,
        node_runtime_options: AgentNodeRuntimeOptions,
    ) -> VibexResult<Self> {
        let db_path = db_path.into();
        let root = root.into();
        if !root.is_absolute() || root.components().any(|part| part == Component::ParentDir) {
            return Err(VibexError::validation(
                "agent_install_root_invalid",
                "managed Agent root must be an absolute path without parent traversal",
            ));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(HTTP_TIMEOUT)
            .redirect(Policy::custom(|attempt| {
                let url = attempt.url();
                if attempt.previous().len() >= 5 {
                    attempt.stop()
                } else if url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .user_agent(concat!("Vibex/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                VibexError::process(
                    "agent_install_http_client_failed",
                    "managed Agent HTTP client could not be initialized",
                )
                .with_diagnostic("error", error.to_string())
            })?;
        let service = Self {
            db_path,
            root,
            config_service,
            node_runtime_options,
            client,
            operation_lock: Arc::new(Mutex::new(())),
        };
        service.recover_interrupted_operations()?;
        Ok(service)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn install(&self, agent_id: AgentId) -> VibexResult<AgentManagedInstallState> {
        let _guard = self.operation_lock.lock().await;
        self.install_locked(agent_id).await
    }

    /// Restores an existing healthy installation during startup without
    /// silently upgrading it. A missing or invalid installation is repaired.
    pub async fn ensure_installed(
        &self,
        agent_id: AgentId,
    ) -> VibexResult<AgentManagedInstallState> {
        let _guard = self.operation_lock.lock().await;
        if let Some(record) = self.read_record(&agent_id)?
            && record_has_usable_installation(&record)
        {
            verify_required_external_commands(&agent_id)?;
            self.config_service.reconcile_agent_acp_runtime(
                agent_id,
                record
                    .command
                    .clone()
                    .expect("usable managed installation has a command"),
            )?;
            return Ok(record.state);
        }
        self.install_locked(agent_id).await
    }

    pub async fn check_update(&self, agent_id: AgentId) -> VibexResult<AgentManagedInstallState> {
        let _guard = self.operation_lock.lock().await;
        let registry_id = require_registry_id(&agent_id)?;
        let registry = self.load_registry(true).await?;
        let entry = registry.require_agent(registry_id)?.clone();
        let distribution = resolve_distribution(&entry)?;
        let now = unix_timestamp_ms();
        let existing = self.read_record(&agent_id)?;
        let existing_is_usable = existing
            .as_ref()
            .is_some_and(record_has_usable_installation);
        let installed_version = existing_is_usable
            .then(|| {
                existing
                    .as_ref()
                    .and_then(|record| record.state.installed_version.clone())
            })
            .flatten();
        let status = match installed_version.as_deref() {
            Some(version) if version_is_newer(version, &entry.version) => {
                AgentManagedInstallStatus::UpdateAvailable
            }
            Some(_) => AgentManagedInstallStatus::Installed,
            None => AgentManagedInstallStatus::NotInstalled,
        };
        let state = AgentManagedInstallState {
            managed: true,
            status,
            distribution_kind: Some(distribution.kind()),
            installed_version,
            available_version: Some(entry.version.clone()),
            last_error_code: None,
            last_error_message: None,
            updated_at_ms: Some(now),
        };
        self.write_record(&AgentManagedInstallationRecord {
            agent_id,
            registry_agent_id: registry_id.to_string(),
            state: state.clone(),
            command: existing.as_ref().and_then(|record| record.command.clone()),
            install_root: existing.and_then(|record| record.install_root),
            updated_at_ms: now,
        })?;
        Ok(state)
    }

    pub async fn uninstall(&self, agent_id: AgentId) -> VibexResult<AgentManagedInstallState> {
        let _guard = self.operation_lock.lock().await;
        require_registry_id(&agent_id)?;
        let previous = self.read_record(&agent_id)?;
        let previous_agent = self
            .config_service
            .list_agents(AgentListRequest {
                include_disabled: true,
            })?
            .agents
            .into_iter()
            .find(|agent| agent.id == agent_id);
        let previous_added = previous_agent.as_ref().is_some_and(|agent| agent.added);
        let previous_enabled = previous_agent.as_ref().is_some_and(|agent| agent.enabled);
        let now = unix_timestamp_ms();
        if let Some(record) = previous.as_ref() {
            let mut state = record.state.clone();
            state.status = AgentManagedInstallStatus::Uninstalling;
            state.updated_at_ms = Some(now);
            self.write_record(&AgentManagedInstallationRecord {
                state,
                updated_at_ms: now,
                ..record.clone()
            })?;
        }

        let result = (|| -> VibexResult<()> {
            self.config_service
                .update_agent_config(AgentUpdateConfigRequest {
                    agent_id: agent_id.clone(),
                    added: Some(false),
                    enabled: Some(false),
                    label_override: None,
                    description_override: None,
                    order_index: None,
                    command: None,
                    env: None,
                    params: None,
                })?;
            let agent_root = self.agent_root(&agent_id)?;
            if fs::symlink_metadata(&agent_root).is_ok() {
                remove_path(&agent_root).map_err(|error| {
                    VibexError::storage(
                        "agent_uninstall_remove_failed",
                        "managed Agent files could not be removed",
                    )
                    .with_diagnostic("causeCode", error.code)
                })?;
            }
            let mut conn = self.open_connection()?;
            apply_migrations(&mut conn)?;
            AgentManagedInstallationRepository::delete(&conn, &agent_id)
        })();

        if let Err(error) = result {
            if let Some(mut record) = previous {
                let installation_is_usable = record_has_usable_installation(&record);
                if installation_is_usable && let Some(command) = record.command.clone() {
                    let restore = self
                        .config_service
                        .reconcile_agent_acp_runtime(agent_id.clone(), command)
                        .and_then(|_| {
                            self.config_service
                                .update_agent_config(AgentUpdateConfigRequest {
                                    agent_id: agent_id.clone(),
                                    added: Some(previous_added),
                                    enabled: Some(previous_enabled),
                                    label_override: None,
                                    description_override: None,
                                    order_index: None,
                                    command: None,
                                    env: None,
                                    params: None,
                                })
                        });
                    if let Err(restore_error) = restore {
                        tracing::warn!(
                            target: "vibex_desktop",
                            error_code = %restore_error.code,
                            "managed Agent configuration rollback failed after uninstall error"
                        );
                    }
                }
                record.state.status = if installation_is_usable {
                    AgentManagedInstallStatus::Installed
                } else {
                    AgentManagedInstallStatus::Failed
                };
                record.state.last_error_code = Some(error.code.clone());
                record.state.last_error_message = Some(error.message.clone());
                record.state.updated_at_ms = Some(unix_timestamp_ms());
                record.updated_at_ms = record.state.updated_at_ms.unwrap_or(now);
                let _ = self.write_record(&record);
            }
            return Err(error);
        }

        let mut state = AgentManagedInstallState::not_installed();
        state.updated_at_ms = Some(unix_timestamp_ms());
        state.available_version = None;
        Ok(state)
    }

    pub fn bootstrap_agent_ids(&self) -> VibexResult<Vec<AgentId>> {
        Ok(self
            .config_service
            .list_agents(AgentListRequest {
                include_disabled: true,
            })?
            .agents
            .into_iter()
            .filter(|agent| agent.added && agent.managed_install.managed)
            .map(|agent| agent.id)
            .collect())
    }

    async fn install_locked(&self, agent_id: AgentId) -> VibexResult<AgentManagedInstallState> {
        let registry_id = require_registry_id(&agent_id)?;
        let registry = self.load_registry(false).await?;
        let entry = registry.require_agent(registry_id)?.clone();
        let distribution = resolve_distribution(&entry)?;
        let distribution_kind = distribution.kind();
        let node_runtime = if matches!(&distribution, ResolvedDistribution::Npm(_)) {
            Some(self.select_node_runtime().await?)
        } else {
            None
        };
        let previous = self.read_record(&agent_id)?;
        let previous_is_usable = previous
            .as_ref()
            .is_some_and(record_has_usable_installation);
        if previous_is_usable
            && let Some(installed_version) = previous
                .as_ref()
                .and_then(|record| record.state.installed_version.as_deref())
        {
            reject_semver_downgrade(installed_version, &entry.version)?;
        }

        if previous.as_ref().is_some_and(|record| {
            record.state.installed_version.as_deref() == Some(entry.version.as_str())
                && record
                    .install_root
                    .as_deref()
                    .is_some_and(|root| Path::new(root).is_dir())
                && record.command.as_ref().is_some_and(command_is_available)
                && node_runtime.as_ref().is_none_or(|runtime| {
                    record.command.as_ref().is_some_and(|command| {
                        Path::new(&command.command) == runtime.node.as_path()
                    })
                })
        }) {
            let mut record = previous.expect("matching managed record exists");
            verify_required_external_commands(&agent_id)?;
            self.config_service.reconcile_agent_acp_runtime(
                agent_id,
                record
                    .command
                    .clone()
                    .expect("matching managed installation has a command"),
            )?;
            record.state.status = AgentManagedInstallStatus::Installed;
            record.state.available_version = Some(entry.version);
            record.state.last_error_code = None;
            record.state.last_error_message = None;
            record.state.updated_at_ms = Some(unix_timestamp_ms());
            record.updated_at_ms = record.state.updated_at_ms.unwrap_or(record.updated_at_ms);
            self.write_record(&record)?;
            return Ok(record.state);
        }

        let now = unix_timestamp_ms();
        let pending_state = AgentManagedInstallState {
            managed: true,
            status: if previous_is_usable {
                AgentManagedInstallStatus::Upgrading
            } else {
                AgentManagedInstallStatus::Installing
            },
            distribution_kind: Some(distribution_kind),
            installed_version: previous
                .as_ref()
                .and_then(|record| record.state.installed_version.clone()),
            available_version: Some(entry.version.clone()),
            last_error_code: None,
            last_error_message: None,
            updated_at_ms: Some(now),
        };
        self.write_record(&AgentManagedInstallationRecord {
            agent_id: agent_id.clone(),
            registry_agent_id: registry_id.to_string(),
            state: pending_state,
            command: previous.as_ref().and_then(|record| record.command.clone()),
            install_root: previous
                .as_ref()
                .and_then(|record| record.install_root.clone()),
            updated_at_ms: now,
        })?;

        let installed = self
            .install_distribution(&agent_id, &entry, distribution, node_runtime)
            .await
            .and_then(|installed| {
                verify_required_external_commands(&agent_id)?;
                Ok(installed)
            });

        match installed {
            Ok(installed) => {
                let now = unix_timestamp_ms();
                let install_root = installed.root.to_string_lossy().into_owned();
                let candidate_state = AgentManagedInstallState {
                    managed: true,
                    status: if previous_is_usable {
                        AgentManagedInstallStatus::Upgrading
                    } else {
                        AgentManagedInstallStatus::Installing
                    },
                    distribution_kind: Some(installed.kind),
                    installed_version: Some(entry.version.clone()),
                    available_version: Some(entry.version.clone()),
                    last_error_code: None,
                    last_error_message: None,
                    updated_at_ms: Some(now),
                };
                self.write_record(&AgentManagedInstallationRecord {
                    agent_id: agent_id.clone(),
                    registry_agent_id: registry_id.to_string(),
                    state: candidate_state,
                    command: Some(installed.command.clone()),
                    install_root: Some(install_root.clone()),
                    updated_at_ms: now,
                })?;
                if let Err(error) = self
                    .config_service
                    .reconcile_agent_acp_runtime(agent_id.clone(), installed.command.clone())
                {
                    let now = unix_timestamp_ms();
                    if previous_is_usable {
                        if let Some(previous_command) =
                            previous.as_ref().and_then(|record| record.command.clone())
                        {
                            let _ = self
                                .config_service
                                .reconcile_agent_acp_runtime(agent_id.clone(), previous_command);
                        }
                        if let Some(mut rollback) = previous.clone() {
                            rollback.state.status = AgentManagedInstallStatus::UpdateAvailable;
                            rollback.state.available_version = Some(entry.version.clone());
                            rollback.state.last_error_code = Some(error.code.clone());
                            rollback.state.last_error_message = Some(error.message.clone());
                            rollback.state.updated_at_ms = Some(now);
                            rollback.updated_at_ms = now;
                            let _ = self.write_record(&rollback);
                        }
                    } else {
                        let _ = self
                            .config_service
                            .update_agent_config(AgentUpdateConfigRequest {
                                agent_id: agent_id.clone(),
                                added: Some(false),
                                enabled: Some(false),
                                label_override: None,
                                description_override: None,
                                order_index: None,
                                command: None,
                                env: None,
                                params: None,
                            });
                        let _ = self.write_record(&AgentManagedInstallationRecord {
                            agent_id: agent_id.clone(),
                            registry_agent_id: registry_id.to_string(),
                            state: AgentManagedInstallState {
                                managed: true,
                                status: AgentManagedInstallStatus::Failed,
                                distribution_kind: Some(installed.kind),
                                installed_version: Some(entry.version.clone()),
                                available_version: Some(entry.version.clone()),
                                last_error_code: Some(error.code.clone()),
                                last_error_message: Some(error.message.clone()),
                                updated_at_ms: Some(now),
                            },
                            command: Some(installed.command.clone()),
                            install_root: Some(install_root),
                            updated_at_ms: now,
                        });
                    }
                    return Err(error);
                }
                let state = AgentManagedInstallState {
                    managed: true,
                    status: AgentManagedInstallStatus::Installed,
                    distribution_kind: Some(installed.kind),
                    installed_version: Some(entry.version.clone()),
                    available_version: Some(entry.version),
                    last_error_code: None,
                    last_error_message: None,
                    updated_at_ms: Some(now),
                };
                self.write_record(&AgentManagedInstallationRecord {
                    agent_id: agent_id.clone(),
                    registry_agent_id: registry_id.to_string(),
                    state: state.clone(),
                    command: Some(installed.command),
                    install_root: Some(install_root),
                    updated_at_ms: now,
                })?;
                if let Err(error) = self.prune_old_versions(
                    &agent_id,
                    &installed.root,
                    previous
                        .as_ref()
                        .and_then(|record| record.install_root.as_deref()),
                ) {
                    tracing::warn!(
                        target: "vibex_desktop",
                        agent_id = %agent_id,
                        error_code = %error.code,
                        "old managed Agent versions could not be pruned"
                    );
                }
                Ok(state)
            }
            Err(error) => {
                let now = unix_timestamp_ms();
                let state = AgentManagedInstallState {
                    managed: true,
                    status: if previous_is_usable {
                        AgentManagedInstallStatus::UpdateAvailable
                    } else {
                        AgentManagedInstallStatus::Failed
                    },
                    distribution_kind: Some(distribution_kind),
                    installed_version: previous
                        .as_ref()
                        .and_then(|record| record.state.installed_version.clone()),
                    available_version: Some(entry.version),
                    last_error_code: Some(error.code.clone()),
                    last_error_message: Some(error.message.clone()),
                    updated_at_ms: Some(now),
                };
                let _ = self.write_record(&AgentManagedInstallationRecord {
                    agent_id,
                    registry_agent_id: registry_id.to_string(),
                    state,
                    command: previous.as_ref().and_then(|record| record.command.clone()),
                    install_root: previous.and_then(|record| record.install_root),
                    updated_at_ms: now,
                });
                Err(error)
            }
        }
    }

    async fn install_distribution(
        &self,
        agent_id: &AgentId,
        entry: &RegistryEntry,
        distribution: ResolvedDistribution,
        node_runtime: Option<NodeRuntime>,
    ) -> VibexResult<InstalledAgent> {
        match distribution {
            ResolvedDistribution::Binary(target) => {
                self.install_binary(agent_id, entry, target).await
            }
            ResolvedDistribution::Npm(npx) => {
                let node_runtime = node_runtime.ok_or_else(|| {
                    VibexError::capability(
                        "agent_node_runtime_unselected",
                        "npm Agent installation has no selected Node.js runtime",
                    )
                })?;
                self.install_npm(agent_id, entry, npx, node_runtime).await
            }
        }
    }

    async fn install_binary(
        &self,
        agent_id: &AgentId,
        entry: &RegistryEntry,
        target: RegistryBinaryTarget,
    ) -> VibexResult<InstalledAgent> {
        validate_https_url(&target.archive, "Agent binary")?;
        let sha256 = validate_sha256(target.sha256.as_deref().ok_or_else(|| {
            VibexError::validation(
                "agent_binary_checksum_missing",
                "ACP Registry binary has no SHA-256 and cannot be trusted",
            )
            .with_diagnostic("registryAgentId", entry.id.clone())
        })?)?;
        let args_identity = serde_json::to_string(&target.args).map_err(|error| {
            VibexError::validation(
                "agent_binary_args_invalid",
                "ACP Registry binary arguments could not be verified",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let fingerprint = distribution_fingerprint(&[
            entry.id.as_str(),
            entry.version.as_str(),
            target.archive.as_str(),
            sha256.as_str(),
            target.cmd.as_str(),
            args_identity.as_str(),
        ]);
        let target_root = self.version_root(agent_id, &entry.version, &fingerprint)?;
        if let Some(installed) = load_or_remove_cached_installation(&target_root, &fingerprint)? {
            return Ok(installed);
        }

        let archive = self.download_verified(&target.archive, &sha256).await?;
        let staging = self.create_staging(agent_id)?;
        let mut staging_guard = StagingGuard::new(staging.clone());
        let archive_url = target.archive.clone();
        let archive_for_extract = archive.clone();
        let staging_for_extract = staging.clone();
        tokio::task::spawn_blocking(move || {
            extract_archive(&archive_for_extract, &archive_url, &staging_for_extract)
        })
        .await
        .map_err(|error| {
            VibexError::process(
                "agent_archive_extract_join_failed",
                "Agent archive extraction task failed",
            )
            .with_diagnostic("error", error.to_string())
        })??;

        let command_rel = safe_relative_path(&target.cmd, "binary command")?;
        let command_path = staging.join(&command_rel);
        ensure_regular_file(&staging, &command_path, "agent_binary_missing")?;
        make_executable(&command_path)?;
        let manifest = InstallManifest {
            registry_agent_id: entry.id.clone(),
            version: entry.version.clone(),
            fingerprint: fingerprint.clone(),
            distribution_kind: AgentManagedDistributionKind::Binary,
            launch: ManifestLaunch::Binary {
                command: command_rel.to_string_lossy().into_owned(),
                args: target.args,
            },
        };
        write_json_private(&staging.join("vibex-install.json"), &manifest)?;
        publish_staging(&staging, &target_root)?;
        staging_guard.disarm();
        load_installed_agent(&target_root, &fingerprint)
    }

    async fn install_npm(
        &self,
        agent_id: &AgentId,
        entry: &RegistryEntry,
        npx: RegistryNpxDistribution,
        node: NodeRuntime,
    ) -> VibexResult<InstalledAgent> {
        let (package, package_version) = parse_exact_npm_spec(&npx.package, &entry.version)?;
        let metadata = self.fetch_npm_metadata(package, package_version).await?;
        let integrity = metadata.dist.integrity.as_deref().ok_or_else(|| {
            VibexError::validation(
                "agent_npm_integrity_missing",
                "npm package metadata has no integrity digest",
            )
            .with_diagnostic("package", package.to_string())
        })?;
        validate_npm_integrity(integrity)?;
        let tarball = validate_canonical_npm_tarball_source(
            metadata.dist.tarball.as_deref().ok_or_else(|| {
                VibexError::validation(
                    "agent_npm_tarball_missing",
                    "npm package metadata has no tarball source",
                )
                .with_diagnostic("package", package.to_string())
            })?,
            package,
            package_version,
        )?;
        let bin_path = select_npm_bin(&metadata, package)?;
        let args_identity = serde_json::to_string(&npx.args).map_err(|error| {
            VibexError::validation(
                "agent_npm_args_invalid",
                "ACP Registry npm arguments could not be verified",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let node_identity = node.fingerprint_identity();
        let fingerprint = distribution_fingerprint(&[
            entry.id.as_str(),
            entry.version.as_str(),
            npx.package.as_str(),
            integrity,
            tarball.as_str(),
            bin_path.as_str(),
            args_identity.as_str(),
            node_identity.as_str(),
        ]);
        let target_root = self.version_root(agent_id, &entry.version, &fingerprint)?;
        if let Some(installed) = load_or_remove_cached_installation(&target_root, &fingerprint)? {
            return Ok(installed);
        }

        let staging = self.create_staging(agent_id)?;
        let mut staging_guard = StagingGuard::new(staging.clone());
        let package_json = serde_json::json!({
            "name": "vibex-managed-acp-agent",
            "private": true,
            "version": "0.0.0",
            "dependencies": { (package): format!("={package_version}") }
        });
        write_json_private(&staging.join("package.json"), &package_json)?;
        let npm_user_config = staging.join("npmrc");
        write_private_file(&npm_user_config, b"")?;
        fs::create_dir_all(self.root.join("cache/npm")).map_err(|error| {
            storage_error(
                "agent_npm_cache_create_failed",
                "managed npm cache could not be created",
                error,
            )
        })?;

        let mut command = node.npm_command();
        command
            .arg("install")
            .arg("--ignore-scripts")
            .arg("--no-audit")
            .arg("--no-fund")
            .arg("--save-exact")
            .arg("--registry=https://registry.npmjs.org/")
            .arg("--")
            .arg(&npx.package)
            .current_dir(&staging)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .env("npm_config_cache", self.root.join("cache/npm"))
            .env("npm_config_userconfig", &npm_user_config)
            .env("npm_config_globalconfig", &npm_user_config)
            .env("npm_config_update_notifier", "false")
            .env_remove("NPM_TOKEN")
            .env_remove("NODE_AUTH_TOKEN");
        let status = timeout(INSTALL_TIMEOUT, command.status())
            .await
            .map_err(|_| {
                VibexError::process(
                    "agent_npm_install_timeout",
                    "managed npm installation timed out",
                )
            })?
            .map_err(|error| {
                process_error(
                    "agent_npm_install_spawn_failed",
                    "managed npm installation could not start",
                    error,
                )
            })?;
        if !status.success() {
            return Err(VibexError::process(
                "agent_npm_install_failed",
                "managed npm installation failed",
            )
            .with_diagnostic("status", status.to_string()));
        }

        verify_npm_lock(&staging, package, package_version, integrity, &tarball)?;
        let package_root = package_directory(&staging, package)?;
        let script = package_root.join(safe_relative_path(&bin_path, "npm bin")?);
        ensure_regular_file(&staging, &script, "agent_npm_bin_missing")?;
        let script_rel = script.strip_prefix(&staging).map_err(|_| {
            VibexError::validation(
                "agent_npm_bin_outside_install",
                "npm executable escaped the managed installation",
            )
        })?;
        let manifest = InstallManifest {
            registry_agent_id: entry.id.clone(),
            version: entry.version.clone(),
            fingerprint: fingerprint.clone(),
            distribution_kind: AgentManagedDistributionKind::Npm,
            launch: ManifestLaunch::Node {
                node: node.node.to_string_lossy().into_owned(),
                script: script_rel.to_string_lossy().into_owned(),
                args: npx.args,
            },
        };
        write_json_private(&staging.join("vibex-install.json"), &manifest)?;
        publish_staging(&staging, &target_root)?;
        staging_guard.disarm();
        load_installed_agent(&target_root, &fingerprint)
    }

    async fn load_registry(&self, force: bool) -> VibexResult<RegistryIndex> {
        let cache_path = self.root.join("registry/registry.json");
        let metadata_path = self.root.join("registry/metadata.json");
        let cached = fs::read(&cache_path).ok();
        if !force
            && cached.is_some()
            && registry_cache_is_fresh(&metadata_path, unix_timestamp_ms())
        {
            return parse_registry(cached.as_deref().unwrap_or_default());
        }

        let fetched = self
            .fetch_limited(ACP_REGISTRY_URL, REGISTRY_MAX_BYTES)
            .await;
        match fetched {
            Ok(bytes) => {
                let registry = parse_registry(&bytes)?;
                write_private_file_atomic(&cache_path, &bytes)?;
                write_json_private_atomic(
                    &metadata_path,
                    &RegistryCacheMetadata {
                        fetched_at_ms: unix_timestamp_ms(),
                    },
                )?;
                Ok(registry)
            }
            Err(error) if force => Err(error),
            Err(error) => cached
                .as_deref()
                .map(parse_registry)
                .transpose()?
                .ok_or(error),
        }
    }

    async fn fetch_npm_metadata(
        &self,
        package: &str,
        version: &str,
    ) -> VibexResult<NpmPackageMetadata> {
        let encoded = url::form_urlencoded::byte_serialize(package.as_bytes()).collect::<String>();
        let url = format!("https://registry.npmjs.org/{encoded}/{version}");
        let bytes = self.fetch_limited(&url, REGISTRY_MAX_BYTES).await?;
        let metadata: NpmPackageMetadata = serde_json::from_slice(&bytes).map_err(|error| {
            VibexError::validation(
                "agent_npm_metadata_invalid",
                "npm package metadata was invalid",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        if metadata.name != package || metadata.version != version {
            return Err(VibexError::validation(
                "agent_npm_metadata_identity_mismatch",
                "npm package metadata did not match the requested package",
            ));
        }
        validate_canonical_npm_tarball_source(
            metadata.dist.tarball.as_deref().ok_or_else(|| {
                VibexError::validation(
                    "agent_npm_tarball_missing",
                    "npm package metadata has no tarball source",
                )
            })?,
            package,
            version,
        )?;
        Ok(metadata)
    }

    async fn fetch_limited(&self, url: &str, max_bytes: usize) -> VibexResult<Vec<u8>> {
        validate_https_url(url, "download")?;
        let response = self.client.get(url).send().await.map_err(|error| {
            VibexError::process("agent_download_failed", "Agent download request failed")
                .with_diagnostic("error", error.to_string())
        })?;
        if !response.status().is_success() {
            return Err(VibexError::process(
                "agent_download_status_failed",
                "Agent download server returned an error",
            )
            .with_diagnostic("status", response.status().as_u16().to_string()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(VibexError::validation(
                "agent_download_too_large",
                "Agent download exceeded the allowed size",
            ));
        }
        let mut bytes = Vec::with_capacity(
            response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or_default()
                .min(max_bytes),
        );
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                VibexError::process(
                    "agent_download_body_failed",
                    "Agent download body could not be read",
                )
                .with_diagnostic("error", error.to_string())
            })?;
            if bytes.len().saturating_add(chunk.len()) > max_bytes {
                return Err(VibexError::validation(
                    "agent_download_too_large",
                    "Agent download exceeded the allowed size",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn download_verified(&self, url: &str, expected_sha256: &str) -> VibexResult<PathBuf> {
        validate_https_url(url, "Agent archive")?;
        let cache_dir = self.root.join("cache/downloads");
        fs::create_dir_all(&cache_dir).map_err(|error| {
            storage_error(
                "agent_download_cache_create_failed",
                "Agent download cache could not be created",
                error,
            )
        })?;
        let cached = cache_dir.join(expected_sha256);
        if cached.is_file() && sha256_file(&cached)? == expected_sha256 {
            return Ok(cached);
        }
        if fs::symlink_metadata(&cached).is_ok() {
            remove_path(&cached).map_err(|error| error.with_diagnostic("cacheKind", "download"))?;
        }

        let response = self.client.get(url).send().await.map_err(|error| {
            VibexError::process("agent_download_failed", "Agent archive download failed")
                .with_diagnostic("error", error.to_string())
        })?;
        if !response.status().is_success() {
            return Err(VibexError::process(
                "agent_download_status_failed",
                "Agent archive server returned an error",
            )
            .with_diagnostic("status", response.status().as_u16().to_string()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > DOWNLOAD_MAX_BYTES)
        {
            return Err(VibexError::validation(
                "agent_download_too_large",
                "Agent archive exceeded the allowed size",
            ));
        }
        let temp = cache_dir.join(format!(
            ".download-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut temp_guard = TempFileGuard::new(temp.clone());
        let mut file = tokio::fs::File::create(&temp).await.map_err(|error| {
            storage_error(
                "agent_download_temp_create_failed",
                "Agent download staging file could not be created",
                error,
            )
        })?;
        let mut stream = response.bytes_stream();
        let mut hasher = Sha256::new();
        let mut written = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                VibexError::process(
                    "agent_download_body_failed",
                    "Agent archive body could not be read",
                )
                .with_diagnostic("error", error.to_string())
            })?;
            written = written.saturating_add(chunk.len() as u64);
            if written > DOWNLOAD_MAX_BYTES {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(VibexError::validation(
                    "agent_download_too_large",
                    "Agent archive exceeded the allowed size",
                ));
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(|error| {
                storage_error(
                    "agent_download_write_failed",
                    "Agent archive could not be cached",
                    error,
                )
            })?;
        }
        file.flush().await.map_err(|error| {
            storage_error(
                "agent_download_flush_failed",
                "Agent archive cache could not be flushed",
                error,
            )
        })?;
        drop(file);
        let actual = format!("{:x}", hasher.finalize());
        if actual != expected_sha256 {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(VibexError::validation(
                "agent_download_checksum_mismatch",
                "Agent archive SHA-256 did not match the Registry",
            )
            .with_diagnostic("expectedSha256", expected_sha256.to_string())
            .with_diagnostic("actualSha256", actual));
        }
        fs::rename(&temp, &cached).map_err(|error| {
            storage_error(
                "agent_download_publish_failed",
                "verified Agent archive could not be published to cache",
                error,
            )
        })?;
        temp_guard.disarm();
        Ok(cached)
    }

    async fn select_node_runtime(&self) -> VibexResult<NodeRuntime> {
        if let Some(runtime) = self.select_external_node_runtime().await {
            return Ok(runtime);
        }
        self.ensure_managed_node_runtime().await
    }

    async fn select_external_node_runtime(&self) -> Option<NodeRuntime> {
        select_valid_external_node_runtime(self.node_runtime_candidates()).await
    }

    fn node_runtime_candidates(&self) -> Vec<NodeRuntimeCandidate> {
        node_runtime_candidates(
            &self.node_runtime_options,
            resolve_system_binary("node"),
            resolve_system_binary("npm"),
        )
    }

    async fn ensure_managed_node_runtime(&self) -> VibexResult<NodeRuntime> {
        let sums = self
            .fetch_limited(NODE_RELEASE_INDEX_URL, 512 * 1024)
            .await?;
        let sums = std::str::from_utf8(&sums).map_err(|error| {
            VibexError::validation(
                "agent_node_checksums_invalid",
                "Node.js checksum index was not UTF-8",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let suffix = node_archive_suffix()?;
        let (sha256, filename) = sums
            .lines()
            .filter_map(|line| line.split_once("  "))
            .find(|(_, filename)| filename.ends_with(suffix))
            .ok_or_else(|| {
                VibexError::capability(
                    "agent_node_platform_unsupported",
                    "Node.js does not publish a runtime for this platform",
                )
            })?;
        let sha256 = validate_sha256(sha256)?;
        let version = filename
            .strip_prefix("node-v")
            .and_then(|value| value.split_once('-').map(|(version, _)| version))
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_node_release_invalid",
                    "Node.js release filename was invalid",
                )
            })?;
        validate_safe_segment(version, "Node.js version")?;
        let version = semver::Version::parse(version).map_err(|error| {
            VibexError::validation(
                "agent_node_release_version_invalid",
                "Node.js release version was invalid",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let platform = current_platform_key()?;
        let target = self
            .root
            .join("runtimes/node")
            .join(version.to_string())
            .join(platform);
        if let Ok(runtime) = node_runtime_at(&target, version.clone()) {
            return Ok(runtime);
        }
        if fs::symlink_metadata(&target).is_ok() {
            remove_path(&target)
                .map_err(|error| error.with_diagnostic("cacheKind", "node-runtime"))?;
        }

        let url = format!("https://nodejs.org/dist/v{version}/{filename}");
        let archive = self.download_verified(&url, &sha256).await?;
        let staging_parent = self.root.join("runtimes/node/.staging");
        fs::create_dir_all(&staging_parent).map_err(|error| {
            storage_error(
                "agent_node_staging_create_failed",
                "Node.js staging directory could not be created",
                error,
            )
        })?;
        let staging = staging_parent.join(format!(
            "{}-{}-{}",
            version,
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&staging).map_err(|error| {
            storage_error(
                "agent_node_staging_create_failed",
                "Node.js staging directory could not be created",
                error,
            )
        })?;
        let mut staging_guard = StagingGuard::new(staging.clone());
        let archive_for_extract = archive.clone();
        let url_for_extract = url.clone();
        let staging_for_extract = staging.clone();
        tokio::task::spawn_blocking(move || {
            extract_archive(&archive_for_extract, &url_for_extract, &staging_for_extract)
        })
        .await
        .map_err(|error| {
            VibexError::process(
                "agent_node_extract_join_failed",
                "Node.js extraction task failed",
            )
            .with_diagnostic("error", error.to_string())
        })??;
        let archive_root_name = strip_archive_suffix(filename);
        let archive_root = staging.join(archive_root_name);
        if !archive_root.is_dir() {
            return Err(VibexError::validation(
                "agent_node_archive_layout_invalid",
                "Node.js archive did not contain its expected root directory",
            ));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                storage_error(
                    "agent_node_runtime_parent_failed",
                    "Node.js runtime directory could not be created",
                    error,
                )
            })?;
        }
        if !target.exists() {
            fs::rename(&archive_root, &target).map_err(|error| {
                storage_error(
                    "agent_node_publish_failed",
                    "verified Node.js runtime could not be published",
                    error,
                )
            })?;
        }
        staging_guard.disarm();
        let _ = fs::remove_dir_all(&staging);
        node_runtime_at(&target, version)
    }

    fn open_connection(&self) -> VibexResult<vibex_db::DbConnection> {
        open_database(&self.db_path)
    }

    fn read_record(
        &self,
        agent_id: &AgentId,
    ) -> VibexResult<Option<AgentManagedInstallationRecord>> {
        let mut conn = self.open_connection()?;
        apply_migrations(&mut conn)?;
        AgentManagedInstallationRepository::get(&conn, agent_id)
    }

    fn write_record(&self, record: &AgentManagedInstallationRecord) -> VibexResult<()> {
        let mut conn = self.open_connection()?;
        apply_migrations(&mut conn)?;
        AgentManagedInstallationRepository::upsert(&conn, record)
    }

    fn recover_interrupted_operations(&self) -> VibexResult<()> {
        let mut conn = self.open_connection()?;
        apply_migrations(&mut conn)?;
        for mut record in AgentManagedInstallationRepository::list(&conn)? {
            if !matches!(
                record.state.status,
                AgentManagedInstallStatus::Installing
                    | AgentManagedInstallStatus::Upgrading
                    | AgentManagedInstallStatus::Uninstalling
            ) {
                continue;
            }
            record.state.status = if installation_files_are_usable(&record)
                && record.state.installed_version.is_some()
            {
                AgentManagedInstallStatus::Installed
            } else {
                AgentManagedInstallStatus::Failed
            };
            record.state.last_error_code = Some("agent_install_interrupted".to_string());
            record.state.last_error_message =
                Some("The previous managed Agent operation was interrupted".to_string());
            record.state.updated_at_ms = Some(unix_timestamp_ms());
            record.updated_at_ms = record.state.updated_at_ms.unwrap_or(record.updated_at_ms);
            AgentManagedInstallationRepository::upsert(&conn, &record)?;
        }
        Ok(())
    }

    fn agent_root(&self, agent_id: &AgentId) -> VibexResult<PathBuf> {
        validate_safe_segment(agent_id.as_str(), "Agent id")?;
        Ok(self.root.join("agents").join(agent_id.as_str()))
    }

    fn version_root(
        &self,
        agent_id: &AgentId,
        version: &str,
        fingerprint: &str,
    ) -> VibexResult<PathBuf> {
        validate_safe_segment(version, "Agent version")?;
        Ok(self
            .agent_root(agent_id)?
            .join(format!("{version}-{}", &fingerprint[..12])))
    }

    fn create_staging(&self, agent_id: &AgentId) -> VibexResult<PathBuf> {
        let agent_root = self.agent_root(agent_id)?;
        fs::create_dir_all(&agent_root).map_err(|error| {
            storage_error(
                "agent_install_directory_create_failed",
                "managed Agent directory could not be created",
                error,
            )
        })?;
        let staging = agent_root.join(format!(
            ".staging-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&staging).map_err(|error| {
            storage_error(
                "agent_install_staging_create_failed",
                "managed Agent staging directory could not be created",
                error,
            )
        })?;
        Ok(staging)
    }

    fn prune_old_versions(
        &self,
        agent_id: &AgentId,
        current: &Path,
        previous: Option<&str>,
    ) -> VibexResult<()> {
        let root = self.agent_root(agent_id)?;
        let mut keep = HashSet::from([current.to_path_buf()]);
        if let Some(previous) = previous {
            let previous = PathBuf::from(previous);
            if previous.starts_with(&root) {
                keep.insert(previous);
            }
        }
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(storage_error(
                    "agent_install_cache_list_failed",
                    "managed Agent cache could not be listed",
                    error,
                ));
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if keep.contains(&path)
                || entry.file_name().to_string_lossy().starts_with(".staging-")
                || !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false)
            {
                continue;
            }
            fs::remove_dir_all(&path).map_err(|error| {
                storage_error(
                    "agent_install_cache_prune_failed",
                    "old managed Agent version could not be removed",
                    error,
                )
            })?;
        }
        Ok(())
    }
}

fn node_runtime_candidates(
    options: &AgentNodeRuntimeOptions,
    system_node: Option<PathBuf>,
    system_npm: Option<PathBuf>,
) -> Vec<NodeRuntimeCandidate> {
    let mut candidates = Vec::new();

    if options.node_path.is_some() || options.npm_path.is_some() {
        let node = options.node_path.clone().or_else(|| system_node.clone());
        let npm = options
            .npm_path
            .clone()
            .or_else(|| node.as_deref().and_then(adjacent_npm_binary))
            .or_else(|| system_npm.clone());
        if let (Some(node), Some(npm)) = (node, npm) {
            candidates.push(NodeRuntimeCandidate {
                source: NodeRuntimeSource::Explicit,
                node,
                npm,
            });
        }
    }

    if let (Some(node), Some(npm)) = (system_node, system_npm) {
        candidates.push(NodeRuntimeCandidate {
            source: NodeRuntimeSource::System,
            node,
            npm,
        });
    }
    candidates
}

async fn select_valid_external_node_runtime(
    candidates: Vec<NodeRuntimeCandidate>,
) -> Option<NodeRuntime> {
    for candidate in candidates {
        match validate_node_runtime_candidate(candidate.clone()).await {
            Ok(runtime) => return Some(runtime),
            Err(error) => tracing::warn!(
                target: "vibex_desktop",
                node_source = candidate.source.as_str(),
                error_code = %error.code,
                "ACP Agent Node.js candidate was rejected"
            ),
        }
    }
    None
}

#[derive(Debug)]
struct InstalledAgent {
    kind: AgentManagedDistributionKind,
    root: PathBuf,
    command: AgentCommandConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct RegistryIndex {
    agents: Vec<RegistryEntry>,
}

impl RegistryIndex {
    fn require_agent(&self, registry_id: &str) -> VibexResult<&RegistryEntry> {
        self.agents
            .iter()
            .find(|entry| entry.id == registry_id)
            .ok_or_else(|| {
                VibexError::capability(
                    "agent_registry_entry_missing",
                    "Agent is not available in the ACP Registry",
                )
                .with_diagnostic("registryAgentId", registry_id.to_string())
            })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RegistryEntry {
    id: String,
    version: String,
    distribution: RegistryDistribution,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RegistryDistribution {
    binary: Option<BTreeMap<String, RegistryBinaryTarget>>,
    npx: Option<RegistryNpxDistribution>,
}

#[derive(Debug, Clone, Deserialize)]
struct RegistryBinaryTarget {
    archive: String,
    cmd: String,
    #[serde(default)]
    args: Vec<String>,
    sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RegistryNpxDistribution {
    package: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Clone)]
enum ResolvedDistribution {
    Binary(RegistryBinaryTarget),
    Npm(RegistryNpxDistribution),
}

impl ResolvedDistribution {
    fn kind(&self) -> AgentManagedDistributionKind {
        match self {
            Self::Binary(_) => AgentManagedDistributionKind::Binary,
            Self::Npm(_) => AgentManagedDistributionKind::Npm,
        }
    }
}

#[derive(Debug, Deserialize)]
struct NpmPackageMetadata {
    name: String,
    version: String,
    dist: NpmDist,
    bin: Option<NpmBin>,
}

#[derive(Debug, Deserialize)]
struct NpmDist {
    integrity: Option<String>,
    tarball: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum NpmBin {
    Single(String),
    Multiple(BTreeMap<String, String>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallManifest {
    registry_agent_id: String,
    version: String,
    fingerprint: String,
    distribution_kind: AgentManagedDistributionKind,
    launch: ManifestLaunch,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ManifestLaunch {
    Binary {
        command: String,
        args: Vec<String>,
    },
    Node {
        node: String,
        script: String,
        args: Vec<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryCacheMetadata {
    fetched_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeRuntimeSource {
    Explicit,
    System,
    Managed,
}

impl NodeRuntimeSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::System => "system",
            Self::Managed => "managed",
        }
    }
}

#[derive(Debug, Clone)]
enum NpmLauncher {
    NodeScript(PathBuf),
    Executable(PathBuf),
}

impl NpmLauncher {
    fn path(&self) -> &Path {
        match self {
            Self::NodeScript(path) | Self::Executable(path) => path,
        }
    }
}

#[derive(Debug, Clone)]
struct NodeRuntime {
    node: PathBuf,
    npm: NpmLauncher,
    version: semver::Version,
    source: NodeRuntimeSource,
}

impl NodeRuntime {
    fn npm_command(&self) -> Command {
        let mut command = match &self.npm {
            NpmLauncher::NodeScript(script) => {
                let mut command = Command::new(&self.node);
                command.arg(script);
                command
            }
            NpmLauncher::Executable(executable) => Command::new(executable),
        };
        prepend_node_to_path(&mut command, &self.node);
        command
    }

    fn fingerprint_identity(&self) -> String {
        format!(
            "{}\0{}\0{}\0{}",
            self.source.as_str(),
            self.version,
            self.node.to_string_lossy(),
            self.npm.path().to_string_lossy()
        )
    }
}

#[derive(Debug, Clone)]
struct NodeRuntimeCandidate {
    source: NodeRuntimeSource,
    node: PathBuf,
    npm: PathBuf,
}

struct StagingGuard {
    path: PathBuf,
    active: bool,
}

struct TempFileGuard {
    path: PathBuf,
    active: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn require_registry_id(agent_id: &AgentId) -> VibexResult<&'static str> {
    acp_registry_agent_id(agent_id).ok_or_else(|| {
        VibexError::capability(
            "agent_managed_install_unavailable",
            "This Agent uses an externally installed CLI",
        )
        .with_diagnostic("agentId", agent_id.as_str().to_string())
    })
}

fn resolve_distribution(entry: &RegistryEntry) -> VibexResult<ResolvedDistribution> {
    if let Some(targets) = entry.distribution.binary.as_ref()
        && let Some(target) = targets.get(current_platform_key()?)
        && target.sha256.as_deref().is_some_and(is_sha256)
    {
        return Ok(ResolvedDistribution::Binary(target.clone()));
    }
    if let Some(npx) = entry.distribution.npx.clone() {
        return Ok(ResolvedDistribution::Npm(npx));
    }
    Err(VibexError::capability(
        "agent_managed_distribution_unavailable",
        "No verified Agent distribution is available for this platform",
    )
    .with_diagnostic("registryAgentId", entry.id.clone()))
}

fn parse_registry(bytes: &[u8]) -> VibexResult<RegistryIndex> {
    let registry: RegistryIndex = serde_json::from_slice(bytes).map_err(|error| {
        VibexError::validation(
            "agent_registry_invalid",
            "ACP Registry response was invalid",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let mut ids = HashSet::new();
    for entry in &registry.agents {
        validate_safe_segment(&entry.id, "Registry Agent id")?;
        validate_safe_segment(&entry.version, "Registry Agent version")?;
        if !ids.insert(entry.id.as_str()) {
            return Err(VibexError::validation(
                "agent_registry_duplicate_id",
                "ACP Registry contains a duplicate Agent id",
            ));
        }
    }
    Ok(registry)
}

fn current_platform_key() -> VibexResult<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        ("macos", "x86_64") => Ok("darwin-x86_64"),
        ("macos", "aarch64") => Ok("darwin-aarch64"),
        ("windows", "x86_64") => Ok("windows-x86_64"),
        ("windows", "aarch64") => Ok("windows-aarch64"),
        _ => Err(VibexError::capability(
            "agent_managed_platform_unsupported",
            "managed Agent installation is unavailable on this platform",
        )),
    }
}

fn node_archive_suffix() -> VibexResult<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("-linux-x64.tar.gz"),
        ("linux", "aarch64") => Ok("-linux-arm64.tar.gz"),
        ("macos", "x86_64") => Ok("-darwin-x64.tar.gz"),
        ("macos", "aarch64") => Ok("-darwin-arm64.tar.gz"),
        ("windows", "x86_64") => Ok("-win-x64.zip"),
        ("windows", "aarch64") => Ok("-win-arm64.zip"),
        _ => Err(VibexError::capability(
            "agent_node_platform_unsupported",
            "managed Node.js is unavailable on this platform",
        )),
    }
}

fn node_runtime_at(root: &Path, version: semver::Version) -> VibexResult<NodeRuntime> {
    #[cfg(windows)]
    let node = root.join("node.exe");
    #[cfg(not(windows))]
    let node = root.join("bin/node");
    #[cfg(windows)]
    let npm_cli = root.join("node_modules/npm/bin/npm-cli.js");
    #[cfg(not(windows))]
    let npm_cli = root.join("lib/node_modules/npm/bin/npm-cli.js");
    if !node.is_file() || !npm_cli.is_file() {
        return Err(VibexError::validation(
            "agent_node_runtime_invalid",
            "managed Node.js runtime is incomplete",
        ));
    }
    Ok(NodeRuntime {
        node,
        npm: NpmLauncher::NodeScript(npm_cli),
        version,
        source: NodeRuntimeSource::Managed,
    })
}

fn nonempty_environment_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn resolve_system_binary(command: &str) -> Option<PathBuf> {
    vibex_config_switch::resolve_binary_path(command).map(PathBuf::from)
}

fn adjacent_npm_binary(node: &Path) -> Option<PathBuf> {
    let parent = node.parent()?;
    #[cfg(windows)]
    let names = ["npm.cmd", "npm.exe", "npm"];
    #[cfg(not(windows))]
    let names = ["npm"];
    names
        .into_iter()
        .map(|name| parent.join(name))
        .find(|candidate| candidate.is_file())
}

async fn validate_node_runtime_candidate(
    candidate: NodeRuntimeCandidate,
) -> VibexResult<NodeRuntime> {
    if !candidate.node.is_file() {
        return Err(VibexError::validation(
            "agent_node_binary_missing",
            "Node.js candidate does not point to a file",
        ));
    }
    if !candidate.npm.is_file() {
        return Err(VibexError::validation(
            "agent_npm_binary_missing",
            "npm candidate does not point to a file",
        ));
    }

    let mut version_command = Command::new(&candidate.node);
    version_command.arg("--version");
    prepend_node_to_path(&mut version_command, &candidate.node);
    let output = run_node_probe(
        version_command,
        "agent_node_version_probe_timeout",
        "agent_node_version_probe_failed",
        "Node.js version could not be detected",
    )
    .await?;
    let version = parse_node_version_output(&output.stdout)?;
    if version < MINIMUM_NODE_VERSION {
        return Err(VibexError::capability(
            "agent_node_version_unsupported",
            "Node.js candidate is older than the supported minimum",
        )
        .with_diagnostic("detectedVersion", version.to_string())
        .with_diagnostic("minimumVersion", MINIMUM_NODE_VERSION.to_string()));
    }

    let npm = external_npm_launcher(&candidate.node, &candidate.npm)?;
    let runtime = NodeRuntime {
        node: candidate.node,
        npm,
        version,
        source: candidate.source,
    };
    let mut npm_command = runtime.npm_command();
    npm_command.arg("--version");
    run_node_probe(
        npm_command,
        "agent_npm_version_probe_timeout",
        "agent_npm_version_probe_failed",
        "npm version could not be detected",
    )
    .await?;
    Ok(runtime)
}

fn external_npm_launcher(_node: &Path, npm: &Path) -> VibexResult<NpmLauncher> {
    if npm.extension().is_some_and(|extension| extension == "js") {
        return Ok(NpmLauncher::NodeScript(npm.to_path_buf()));
    }
    #[cfg(windows)]
    if npm.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    }) {
        let npm_cli = _node
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("node_modules/npm/bin/npm-cli.js");
        if !npm_cli.is_file() {
            return Err(VibexError::validation(
                "agent_npm_cli_missing",
                "npm command script has no adjacent npm CLI",
            ));
        }
        return Ok(NpmLauncher::NodeScript(npm_cli));
    }
    Ok(NpmLauncher::Executable(npm.to_path_buf()))
}

async fn run_node_probe(
    mut command: Command,
    timeout_code: &str,
    failure_code: &str,
    message: &str,
) -> VibexResult<Output> {
    let output = timeout(NODE_PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| VibexError::process(timeout_code, message))?
        .map_err(|error| process_error(failure_code, message, error))?;
    if !output.status.success() {
        return Err(VibexError::process(failure_code, message)
            .with_diagnostic("status", output.status.to_string()));
    }
    Ok(output)
}

fn parse_node_version_output(output: &[u8]) -> VibexResult<semver::Version> {
    let output = std::str::from_utf8(output).map_err(|error| {
        VibexError::validation(
            "agent_node_version_output_invalid",
            "Node.js version output was not UTF-8",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    semver::Version::parse(output.trim().trim_start_matches('v')).map_err(|error| {
        VibexError::validation(
            "agent_node_version_output_invalid",
            "Node.js version output was invalid",
        )
        .with_diagnostic("error", error.to_string())
    })
}

fn prepend_node_to_path(command: &mut Command, node: &Path) {
    let Some(node_directory) = node.parent() else {
        return;
    };
    let paths = std::iter::once(node_directory.to_path_buf()).chain(
        env::var_os("PATH")
            .into_iter()
            .flat_map(|value| env::split_paths(&value).collect::<Vec<_>>()),
    );
    if let Ok(path) = env::join_paths(paths) {
        command.env("PATH", path);
    }
}

fn parse_exact_npm_spec<'a>(spec: &'a str, expected: &str) -> VibexResult<(&'a str, &'a str)> {
    let (package, version) = spec.rsplit_once('@').ok_or_else(|| {
        VibexError::validation(
            "agent_npm_spec_not_exact",
            "ACP Registry npm package must include an exact version",
        )
    })?;
    if package.is_empty()
        || version.is_empty()
        || version != expected
        || package.contains("..")
        || !package.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '@' | '/' | '-' | '_' | '.')
        })
    {
        return Err(VibexError::validation(
            "agent_npm_spec_invalid",
            "ACP Registry npm package identity was invalid",
        ));
    }
    semver::Version::parse(version).map_err(|error| {
        VibexError::validation(
            "agent_npm_version_invalid",
            "ACP Registry npm version was not exact semver",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    Ok((package, version))
}

fn select_npm_bin(metadata: &NpmPackageMetadata, package: &str) -> VibexResult<String> {
    let bin = metadata.bin.as_ref().ok_or_else(|| {
        VibexError::validation(
            "agent_npm_bin_missing",
            "npm package does not declare an executable",
        )
    })?;
    let selected = match bin {
        NpmBin::Single(path) => path,
        NpmBin::Multiple(bins) => {
            let preferred = package.rsplit('/').next().unwrap_or(package);
            bins.get(preferred)
                .or_else(|| (bins.len() == 1).then(|| bins.values().next()).flatten())
                .ok_or_else(|| {
                    VibexError::validation(
                        "agent_npm_bin_missing",
                        "npm package did not declare an unambiguous executable",
                    )
                })?
        }
    };
    let _ = safe_relative_path(selected, "npm bin")?;
    Ok(selected.clone())
}

fn canonical_npm_tarball_url(package: &str, version: &str) -> VibexResult<String> {
    let unscoped_name = if let Some(scoped) = package.strip_prefix('@') {
        let (scope, name) = scoped.split_once('/').ok_or_else(|| {
            VibexError::validation(
                "agent_npm_package_name_invalid",
                "scoped npm package identity was invalid",
            )
        })?;
        if scope.is_empty() || name.is_empty() || name.contains('/') {
            return Err(VibexError::validation(
                "agent_npm_package_name_invalid",
                "scoped npm package identity was invalid",
            ));
        }
        name
    } else {
        if package.is_empty() || package.contains('/') || package.contains('@') {
            return Err(VibexError::validation(
                "agent_npm_package_name_invalid",
                "npm package identity was invalid",
            ));
        }
        package
    };
    semver::Version::parse(version).map_err(|error| {
        VibexError::validation(
            "agent_npm_version_invalid",
            "npm package version was not exact semver",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    Ok(format!(
        "https://registry.npmjs.org/{package}/-/{unscoped_name}-{version}.tgz"
    ))
}

fn validate_canonical_npm_tarball_source(
    source: &str,
    package: &str,
    version: &str,
) -> VibexResult<String> {
    let expected = canonical_npm_tarball_url(package, version)?;
    let parsed = Url::parse(source).map_err(|error| {
        VibexError::validation(
            "agent_npm_tarball_source_invalid",
            "npm tarball source URL was invalid",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    if source != expected
        || parsed.scheme() != "https"
        || parsed.host_str() != Some("registry.npmjs.org")
        || parsed.port().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(VibexError::validation(
            "agent_npm_tarball_source_mismatch",
            "npm tarball source did not match the canonical public registry URL",
        )
        .with_diagnostic("expected", expected)
        .with_diagnostic("actual", source.to_string()));
    }
    Ok(expected)
}

fn verify_npm_lock(
    root: &Path,
    package: &str,
    version: &str,
    integrity: &str,
    resolved: &str,
) -> VibexResult<()> {
    let lock_bytes = fs::read(root.join("package-lock.json")).map_err(|error| {
        storage_error(
            "agent_npm_lock_missing",
            "npm did not produce a package lock",
            error,
        )
    })?;
    let lock: serde_json::Value = serde_json::from_slice(&lock_bytes).map_err(|error| {
        VibexError::validation("agent_npm_lock_invalid", "npm package lock was invalid")
            .with_diagnostic("error", error.to_string())
    })?;
    let key = format!("node_modules/{package}");
    let locked = lock
        .get("packages")
        .and_then(|packages| packages.get(&key))
        .ok_or_else(|| {
            VibexError::validation(
                "agent_npm_lock_package_missing",
                "npm package lock omitted the requested Agent package",
            )
        })?;
    if locked.get("version").and_then(|value| value.as_str()) != Some(version)
        || locked.get("integrity").and_then(|value| value.as_str()) != Some(integrity)
        || locked.get("resolved").and_then(|value| value.as_str()) != Some(resolved)
    {
        return Err(VibexError::validation(
            "agent_npm_lock_identity_mismatch",
            "npm package lock did not match the trusted package identity",
        ));
    }
    Ok(())
}

fn package_directory(root: &Path, package: &str) -> VibexResult<PathBuf> {
    let mut path = root.join("node_modules");
    let parts = package.split('/').collect::<Vec<_>>();
    if parts.is_empty()
        || parts.len() > 2
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(VibexError::validation(
            "agent_npm_package_path_invalid",
            "npm package path was invalid",
        ));
    }
    for part in parts {
        path.push(part);
    }
    Ok(path)
}

fn validate_npm_integrity(integrity: &str) -> VibexResult<()> {
    let digest = integrity
        .strip_prefix("sha512-")
        .and_then(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
        })
        .filter(|digest| digest.len() == 64);
    if digest.is_none() {
        return Err(VibexError::validation(
            "agent_npm_integrity_invalid",
            "npm package integrity digest was invalid",
        ));
    }
    Ok(())
}

fn load_installed_agent(root: &Path, expected_fingerprint: &str) -> VibexResult<InstalledAgent> {
    let bytes = fs::read(root.join("vibex-install.json")).map_err(|error| {
        storage_error(
            "agent_install_manifest_missing",
            "managed Agent manifest could not be read",
            error,
        )
    })?;
    let manifest: InstallManifest = serde_json::from_slice(&bytes).map_err(|error| {
        VibexError::validation(
            "agent_install_manifest_invalid",
            "managed Agent manifest was invalid",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    if manifest.fingerprint != expected_fingerprint {
        return Err(VibexError::validation(
            "agent_install_fingerprint_mismatch",
            "managed Agent cache did not match the requested distribution",
        ));
    }
    let command = match manifest.launch {
        ManifestLaunch::Binary { command, args } => {
            let path = root.join(safe_relative_path(&command, "binary command")?);
            ensure_regular_file(root, &path, "agent_binary_missing")?;
            AgentCommandConfig {
                command: path.to_string_lossy().into_owned(),
                args,
            }
        }
        ManifestLaunch::Node { node, script, args } => {
            let node = PathBuf::from(node);
            let script = root.join(safe_relative_path(&script, "npm script")?);
            ensure_regular_file(root, &script, "agent_npm_bin_missing")?;
            if !node.is_file() {
                return Err(VibexError::validation(
                    "agent_node_runtime_missing",
                    "managed Node.js runtime is missing",
                ));
            }
            let mut launch_args = vec![script.to_string_lossy().into_owned()];
            launch_args.extend(args);
            AgentCommandConfig {
                command: node.to_string_lossy().into_owned(),
                args: launch_args,
            }
        }
    };
    Ok(InstalledAgent {
        kind: manifest.distribution_kind,
        root: root.to_path_buf(),
        command,
    })
}

fn load_or_remove_cached_installation(
    root: &Path,
    expected_fingerprint: &str,
) -> VibexResult<Option<InstalledAgent>> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(storage_error(
                "agent_install_cache_metadata_failed",
                "managed Agent installation cache could not be inspected",
                error,
            ));
        }
    };
    if metadata.file_type().is_dir()
        && let Ok(installed) = load_installed_agent(root, expected_fingerprint)
    {
        return Ok(Some(installed));
    }
    remove_path(root).map_err(|error| error.with_diagnostic("cacheKind", "agent-installation"))?;
    Ok(None)
}

fn remove_path(path: &Path) -> VibexResult<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(storage_error(
                "agent_install_cache_metadata_failed",
                "managed Agent cache entry could not be inspected",
                error,
            ));
        }
    };
    let result = if metadata.file_type().is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    if let Err(error) = result {
        return Err(storage_error(
            "agent_install_cache_repair_failed",
            "invalid managed Agent cache entry could not be replaced",
            error,
        ));
    }
    Ok(())
}

fn record_has_usable_installation(record: &AgentManagedInstallationRecord) -> bool {
    record.state.has_usable_installation() && installation_files_are_usable(record)
}

fn installation_files_are_usable(record: &AgentManagedInstallationRecord) -> bool {
    record
        .install_root
        .as_deref()
        .is_some_and(|root| Path::new(root).is_dir())
        && record.command.as_ref().is_some_and(command_is_available)
}

fn reject_semver_downgrade(installed: &str, available: &str) -> VibexResult<()> {
    let (Ok(installed_version), Ok(available_version)) = (
        semver::Version::parse(installed),
        semver::Version::parse(available),
    ) else {
        return Ok(());
    };
    if available_version < installed_version {
        return Err(VibexError::conflict(
            "agent_install_downgrade_rejected",
            "ACP Registry candidate is older than the installed Agent",
        )
        .with_diagnostic("installedVersion", installed.to_string())
        .with_diagnostic("availableVersion", available.to_string()));
    }
    Ok(())
}

fn version_is_newer(installed: &str, available: &str) -> bool {
    match (
        semver::Version::parse(installed),
        semver::Version::parse(available),
    ) {
        (Ok(installed), Ok(available)) => available > installed,
        _ => installed != available,
    }
}

fn command_is_available(command: &AgentCommandConfig) -> bool {
    let program = Path::new(&command.command);
    program.is_file()
        && command
            .args
            .first()
            .filter(|_| program.file_stem().is_some_and(|name| name == "node"))
            .is_none_or(|script| Path::new(script).is_file())
}

fn verify_required_external_commands(agent_id: &AgentId) -> VibexResult<()> {
    let required: &[&str] = match agent_id.as_str() {
        "amp-acp" => &["amp"],
        "autohand" => &["autohand"],
        _ => &[],
    };
    for command in required {
        if vibex_config_switch::resolve_binary_path(command).is_none() {
            return Err(VibexError::capability(
                "agent_required_cli_missing",
                "The ACP adapter was installed, but its required Agent CLI is missing",
            )
            .with_diagnostic("requiredCommand", (*command).to_string()));
        }
    }
    Ok(())
}

fn distribution_fingerprint(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

fn validate_https_url(value: &str, label: &str) -> VibexResult<Url> {
    let url = Url::parse(value).map_err(|error| {
        VibexError::validation(
            "agent_download_url_invalid",
            format!("{label} URL was invalid"),
        )
        .with_diagnostic("error", error.to_string())
    })?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(VibexError::validation(
            "agent_download_url_untrusted",
            format!("{label} URL must use HTTPS without embedded credentials"),
        ));
    }
    Ok(url)
}

fn validate_sha256(value: &str) -> VibexResult<String> {
    if is_sha256(value) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err(VibexError::validation(
            "agent_binary_checksum_invalid",
            "Agent binary SHA-256 was invalid",
        ))
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_safe_segment(value: &str, label: &str) -> VibexResult<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
    {
        return Err(VibexError::validation(
            "agent_install_path_segment_invalid",
            format!("{label} was not a safe path segment"),
        ));
    }
    Ok(())
}

fn safe_relative_path(value: &str, label: &str) -> VibexResult<PathBuf> {
    let path = Path::new(value);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(VibexError::validation(
                    "agent_install_relative_path_invalid",
                    format!("{label} escaped the managed installation"),
                ));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(VibexError::validation(
            "agent_install_relative_path_empty",
            format!("{label} was empty"),
        ));
    }
    Ok(clean)
}

fn ensure_regular_file(root: &Path, path: &Path, code: &str) -> VibexResult<()> {
    if !path.starts_with(root) || !path.is_file() {
        return Err(VibexError::validation(
            code,
            "managed Agent executable was missing",
        ));
    }
    let canonical_root = fs::canonicalize(root).map_err(|error| {
        storage_error(
            "agent_install_root_canonicalize_failed",
            "managed Agent root could not be verified",
            error,
        )
    })?;
    let canonical_path = fs::canonicalize(path).map_err(|error| {
        storage_error(
            "agent_install_file_canonicalize_failed",
            "managed Agent executable could not be verified",
            error,
        )
    })?;
    if !canonical_path.starts_with(canonical_root) {
        return Err(VibexError::validation(
            "agent_install_symlink_escape",
            "managed Agent executable escaped its installation",
        ));
    }
    Ok(())
}

fn publish_staging(staging: &Path, target: &Path) -> VibexResult<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            storage_error(
                "agent_install_parent_create_failed",
                "managed Agent version directory could not be created",
                error,
            )
        })?;
    }
    if target.exists() {
        fs::remove_dir_all(staging).map_err(|error| {
            storage_error(
                "agent_install_staging_cleanup_failed",
                "redundant managed Agent staging directory could not be removed",
                error,
            )
        })?;
        return Ok(());
    }
    fs::rename(staging, target).map_err(|error| {
        storage_error(
            "agent_install_publish_failed",
            "verified Agent installation could not be published",
            error,
        )
    })
}

fn extract_archive(archive: &Path, source_url: &str, destination: &Path) -> VibexResult<()> {
    let url = Url::parse(source_url).map_err(|error| {
        VibexError::validation("agent_archive_url_invalid", "Agent archive URL was invalid")
            .with_diagnostic("error", error.to_string())
    })?;
    let path = url.path().to_ascii_lowercase();
    if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
        extract_tar_gz(archive, destination)
    } else if path.ends_with(".tar.bz2") || path.ends_with(".tbz2") {
        extract_tar_bz2(archive, destination)
    } else if path.ends_with(".zip") {
        extract_zip(archive, destination)
    } else if [
        ".dmg",
        ".pkg",
        ".deb",
        ".rpm",
        ".msi",
        ".appimage",
        ".tar.xz",
        ".txz",
        ".tar",
        ".gz",
        ".bz2",
        ".xz",
        ".7z",
    ]
    .iter()
    .any(|suffix| path.ends_with(suffix))
    {
        Err(VibexError::capability(
            "agent_archive_format_unsupported",
            "Agent archive format is not supported",
        ))
    } else {
        let name = url
            .path_segments()
            .and_then(|mut segments| segments.rfind(|part| !part.is_empty()))
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_archive_filename_missing",
                    "raw Agent binary URL had no filename",
                )
            })?;
        validate_safe_segment(name, "raw Agent binary filename")?;
        fs::copy(archive, destination.join(name)).map_err(|error| {
            storage_error(
                "agent_raw_binary_copy_failed",
                "raw Agent binary could not be staged",
                error,
            )
        })?;
        Ok(())
    }
}

fn extract_tar_gz(archive: &Path, destination: &Path) -> VibexResult<()> {
    let file = open_archive(archive)?;
    extract_tar(GzDecoder::new(file), destination)
}

fn extract_tar_bz2(archive: &Path, destination: &Path) -> VibexResult<()> {
    let file = open_archive(archive)?;
    extract_tar(BzDecoder::new(file), destination)
}

fn open_archive(archive: &Path) -> VibexResult<File> {
    File::open(archive).map_err(|error| {
        storage_error(
            "agent_archive_open_failed",
            "Agent archive could not be opened",
            error,
        )
    })
}

fn extract_tar(reader: impl Read, destination: &Path) -> VibexResult<()> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries().map_err(|error| {
        storage_error(
            "agent_archive_read_failed",
            "Agent tar archive could not be read",
            error,
        )
    })?;
    let mut entry_count = 0_usize;
    let mut unpacked_bytes = 0_u64;
    for entry in entries {
        let mut entry = entry.map_err(|error| {
            storage_error(
                "agent_archive_entry_invalid",
                "Agent tar entry was invalid",
                error,
            )
        })?;
        entry_count = entry_count.saturating_add(1);
        check_archive_budget(entry_count, unpacked_bytes)?;
        let relative = entry.path().map_err(|error| {
            storage_error(
                "agent_archive_path_invalid",
                "Agent tar entry path was invalid",
                error,
            )
        })?;
        let relative = safe_relative_path(&relative.to_string_lossy(), "archive entry")?;
        let output = destination.join(relative);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&output).map_err(|error| {
                storage_error(
                    "agent_archive_directory_failed",
                    "Agent archive directory could not be created",
                    error,
                )
            })?;
        } else if kind.is_file() {
            unpacked_bytes = unpacked_bytes.checked_add(entry.size()).ok_or_else(|| {
                VibexError::validation(
                    "agent_archive_unpacked_too_large",
                    "Agent archive expanded beyond the allowed size",
                )
            })?;
            check_archive_budget(entry_count, unpacked_bytes)?;
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    storage_error(
                        "agent_archive_directory_failed",
                        "Agent archive directory could not be created",
                        error,
                    )
                })?;
            }
            entry.unpack(&output).map_err(|error| {
                storage_error(
                    "agent_archive_extract_failed",
                    "Agent tar entry could not be extracted",
                    error,
                )
            })?;
        }
    }
    Ok(())
}

fn extract_zip(archive: &Path, destination: &Path) -> VibexResult<()> {
    let file = open_archive(archive)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        VibexError::validation("agent_zip_invalid", "Agent zip archive was invalid")
            .with_diagnostic("error", error.to_string())
    })?;
    let mut unpacked_bytes = 0_u64;
    for index in 0..archive.len() {
        check_archive_budget(index.saturating_add(1), unpacked_bytes)?;
        let mut entry = archive.by_index(index).map_err(|error| {
            VibexError::validation("agent_zip_entry_invalid", "Agent zip entry was invalid")
                .with_diagnostic("error", error.to_string())
        })?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            continue;
        }
        let relative = entry.enclosed_name().ok_or_else(|| {
            VibexError::validation(
                "agent_zip_path_escape",
                "Agent zip entry escaped the installation",
            )
        })?;
        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&output).map_err(|error| {
                storage_error(
                    "agent_archive_directory_failed",
                    "Agent archive directory could not be created",
                    error,
                )
            })?;
            continue;
        }
        unpacked_bytes = unpacked_bytes.checked_add(entry.size()).ok_or_else(|| {
            VibexError::validation(
                "agent_archive_unpacked_too_large",
                "Agent archive expanded beyond the allowed size",
            )
        })?;
        check_archive_budget(index.saturating_add(1), unpacked_bytes)?;
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                storage_error(
                    "agent_archive_directory_failed",
                    "Agent archive directory could not be created",
                    error,
                )
            })?;
        }
        let mut output_file = File::create(&output).map_err(|error| {
            storage_error(
                "agent_zip_output_failed",
                "Agent zip output file could not be created",
                error,
            )
        })?;
        io::copy(&mut entry, &mut output_file).map_err(|error| {
            storage_error(
                "agent_zip_extract_failed",
                "Agent zip entry could not be extracted",
                error,
            )
        })?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&output, fs::Permissions::from_mode(mode & 0o777)).map_err(
                |error| {
                    storage_error(
                        "agent_zip_permissions_failed",
                        "Agent zip permissions could not be applied",
                        error,
                    )
                },
            )?;
        }
    }
    Ok(())
}

fn check_archive_budget(entry_count: usize, unpacked_bytes: u64) -> VibexResult<()> {
    if entry_count > ARCHIVE_MAX_ENTRIES {
        return Err(VibexError::validation(
            "agent_archive_too_many_entries",
            "Agent archive contained too many entries",
        ));
    }
    if unpacked_bytes > ARCHIVE_MAX_UNPACKED_BYTES {
        return Err(VibexError::validation(
            "agent_archive_unpacked_too_large",
            "Agent archive expanded beyond the allowed size",
        ));
    }
    Ok(())
}

fn make_executable(path: &Path) -> VibexResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = fs::metadata(path)
            .map_err(|error| {
                storage_error(
                    "agent_binary_metadata_failed",
                    "Agent binary metadata could not be read",
                    error,
                )
            })?
            .permissions();
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(path, permissions).map_err(|error| {
            storage_error(
                "agent_binary_permissions_failed",
                "Agent binary could not be made executable",
                error,
            )
        })?;
    }
    Ok(())
}

fn sha256_file(path: &Path) -> VibexResult<String> {
    let mut file = File::open(path).map_err(|error| {
        storage_error(
            "agent_cache_read_failed",
            "Agent cache entry could not be read",
            error,
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            storage_error(
                "agent_cache_read_failed",
                "Agent cache entry could not be read",
                error,
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn registry_cache_is_fresh(path: &Path, now: i64) -> bool {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RegistryCacheMetadata>(&bytes).ok())
        .and_then(|metadata| now.checked_sub(metadata.fetched_at_ms))
        .is_some_and(|age| (0..=REGISTRY_CACHE_MAX_AGE_MS).contains(&age))
}

fn strip_archive_suffix(filename: &str) -> &str {
    filename
        .strip_suffix(".tar.gz")
        .or_else(|| filename.strip_suffix(".tgz"))
        .or_else(|| filename.strip_suffix(".tar.bz2"))
        .or_else(|| filename.strip_suffix(".tbz2"))
        .or_else(|| filename.strip_suffix(".zip"))
        .unwrap_or(filename)
}

fn write_json_private(path: &Path, value: &impl Serialize) -> VibexResult<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        VibexError::storage(
            "agent_install_manifest_encode_failed",
            "managed Agent metadata could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    write_private_file(path, &bytes)
}

fn write_json_private_atomic(path: &Path, value: &impl Serialize) -> VibexResult<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        VibexError::storage(
            "agent_install_metadata_encode_failed",
            "managed Agent metadata could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    write_private_file_atomic(path, &bytes)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> VibexResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            storage_error(
                "agent_install_directory_create_failed",
                "managed Agent directory could not be created",
                error,
            )
        })?;
    }
    let mut file = File::create(path).map_err(|error| {
        storage_error(
            "agent_install_file_create_failed",
            "managed Agent file could not be created",
            error,
        )
    })?;
    file.write_all(bytes).map_err(|error| {
        storage_error(
            "agent_install_file_write_failed",
            "managed Agent file could not be written",
            error,
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            storage_error(
                "agent_install_file_permissions_failed",
                "managed Agent file permissions could not be restricted",
                error,
            )
        })?;
    }
    Ok(())
}

fn write_private_file_atomic(path: &Path, bytes: &[u8]) -> VibexResult<()> {
    let parent = path.parent().ok_or_else(|| {
        VibexError::validation(
            "agent_install_atomic_parent_missing",
            "managed Agent metadata path had no parent",
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        storage_error(
            "agent_install_directory_create_failed",
            "managed Agent directory could not be created",
            error,
        )
    })?;
    let temp = parent.join(format!(
        ".metadata-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut temp_guard = TempFileGuard::new(temp.clone());
    write_private_file(&temp, bytes)?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path).map_err(|error| {
            storage_error(
                "agent_install_metadata_replace_failed",
                "old managed Agent metadata could not be replaced",
                error,
            )
        })?;
    }
    fs::rename(&temp, path).map_err(|error| {
        storage_error(
            "agent_install_metadata_publish_failed",
            "managed Agent metadata could not be published",
            error,
        )
    })?;
    temp_guard.disarm();
    Ok(())
}

fn storage_error(code: &str, message: &str, error: impl ToString) -> VibexError {
    VibexError::storage(code, message).with_diagnostic("error", error.to_string())
}

fn process_error(code: &str, message: &str, error: impl ToString) -> VibexError {
    VibexError::process(code, message).with_diagnostic("error", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn write_version_probe(path: &Path, version: &str) {
        use std::os::unix::fs::PermissionsExt as _;

        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf '%s\\n' '{version}'; exit 0; fi\nexit 1\n"
        );
        fs::write(path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn node_runtime_selection_prefers_valid_explicit_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let explicit_node = temp.path().join("explicit-node");
        let explicit_npm = temp.path().join("explicit-npm");
        let system_node = temp.path().join("system-node");
        let system_npm = temp.path().join("system-npm");
        write_version_probe(&explicit_node, "v22.14.0");
        write_version_probe(&explicit_npm, "10.9.2");
        write_version_probe(&system_node, "v22.13.0");
        write_version_probe(&system_npm, "10.9.1");

        let options = AgentNodeRuntimeOptions {
            node_path: Some(explicit_node.clone()),
            npm_path: Some(explicit_npm),
        };
        let candidates = node_runtime_candidates(&options, Some(system_node), Some(system_npm));
        let runtime = select_valid_external_node_runtime(candidates)
            .await
            .expect("explicit runtime should be selected");
        assert_eq!(runtime.source, NodeRuntimeSource::Explicit);
        assert_eq!(runtime.node, explicit_node);
        assert_eq!(runtime.version, semver::Version::new(22, 14, 0));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_explicit_node_falls_back_to_valid_system_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let explicit_node = temp.path().join("explicit-node");
        let explicit_npm = temp.path().join("explicit-npm");
        let system_node = temp.path().join("system-node");
        let system_npm = temp.path().join("system-npm");
        write_version_probe(&explicit_node, "v18.20.4");
        write_version_probe(&explicit_npm, "10.9.2");
        write_version_probe(&system_node, "v22.14.0");
        write_version_probe(&system_npm, "10.9.2");

        let options = AgentNodeRuntimeOptions {
            node_path: Some(explicit_node),
            npm_path: Some(explicit_npm),
        };
        let runtime = select_valid_external_node_runtime(node_runtime_candidates(
            &options,
            Some(system_node.clone()),
            Some(system_npm),
        ))
        .await
        .expect("system runtime should be selected");
        assert_eq!(runtime.source, NodeRuntimeSource::System);
        assert_eq!(runtime.node, system_node);
        assert_eq!(runtime.version, semver::Version::new(22, 14, 0));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_explicit_and_system_runtime_candidates_allow_managed_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let explicit_node = temp.path().join("explicit-node");
        let explicit_npm = temp.path().join("explicit-npm");
        let system_node = temp.path().join("system-node");
        let system_npm = temp.path().join("system-npm");
        write_version_probe(&explicit_node, "v18.20.4");
        write_version_probe(&explicit_npm, "10.9.2");
        write_version_probe(&system_node, "v20.19.0");
        write_version_probe(&system_npm, "10.9.2");

        let options = AgentNodeRuntimeOptions {
            node_path: Some(explicit_node),
            npm_path: Some(explicit_npm),
        };
        assert!(
            select_valid_external_node_runtime(node_runtime_candidates(
                &options,
                Some(system_node),
                Some(system_npm),
            ))
            .await
            .is_none()
        );
    }

    #[test]
    fn node_version_probe_parses_v_prefix_and_rejects_malformed_output() {
        assert_eq!(
            parse_node_version_output(b"v22.14.0\n").unwrap(),
            semver::Version::new(22, 14, 0)
        );
        assert!(parse_node_version_output(b"node-22\n").is_err());
    }

    #[test]
    fn registry_aliases_and_external_agents_are_explicit() {
        assert_eq!(
            require_registry_id(&AgentId::parse("claude").unwrap()).unwrap(),
            "claude-acp"
        );
        assert_eq!(
            require_registry_id(&AgentId::parse("copilot").unwrap()).unwrap(),
            "github-copilot-cli"
        );
        assert!(require_registry_id(&AgentId::parse("cursor").unwrap()).is_err());
        assert!(require_registry_id(&AgentId::parse("fast-agent").unwrap()).is_err());
    }

    #[test]
    fn exact_npm_specs_reject_ranges_and_identity_drift() {
        assert_eq!(
            parse_exact_npm_spec("@scope/agent@1.2.3", "1.2.3").unwrap(),
            ("@scope/agent", "1.2.3")
        );
        assert!(parse_exact_npm_spec("agent@^1.2.3", "1.2.3").is_err());
        assert!(parse_exact_npm_spec("agent@1.2.4", "1.2.3").is_err());
    }

    #[test]
    fn distribution_requires_platform_checksum_or_exact_npm_fallback() {
        let key = current_platform_key().unwrap().to_string();
        let entry = RegistryEntry {
            id: "test-agent".to_string(),
            version: "1.2.3".to_string(),
            distribution: RegistryDistribution {
                binary: Some(BTreeMap::from([(
                    key.clone(),
                    RegistryBinaryTarget {
                        archive: "https://example.com/agent.tar.gz".to_string(),
                        cmd: "./agent".to_string(),
                        args: Vec::new(),
                        sha256: None,
                    },
                )])),
                npx: None,
            },
        };
        assert!(resolve_distribution(&entry).is_err());

        let fallback = RegistryEntry {
            distribution: RegistryDistribution {
                npx: Some(RegistryNpxDistribution {
                    package: "test-agent@1.2.3".to_string(),
                    args: Vec::new(),
                }),
                ..entry.distribution.clone()
            },
            ..entry
        };
        assert!(matches!(
            resolve_distribution(&fallback).unwrap(),
            ResolvedDistribution::Npm(_)
        ));
    }

    #[test]
    fn relative_archive_paths_cannot_escape_installation() {
        assert_eq!(
            safe_relative_path("./bin/agent", "command").unwrap(),
            PathBuf::from("bin/agent")
        );
        assert!(safe_relative_path("../agent", "command").is_err());
        assert!(safe_relative_path("/usr/bin/agent", "command").is_err());
    }

    #[test]
    fn checksum_validation_is_fail_closed() {
        assert!(is_sha256(&"a".repeat(64)));
        assert!(!is_sha256(&"g".repeat(64)));
        assert!(!is_sha256("abc"));
        assert_eq!(validate_sha256(&"A".repeat(64)).unwrap(), "a".repeat(64));
        let npm_integrity = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode([0_u8; 64])
        );
        assert!(validate_npm_integrity(&npm_integrity).is_ok());
        assert!(validate_npm_integrity("sha512-not-base64").is_err());
    }

    #[test]
    fn update_checks_do_not_offer_semver_downgrades() {
        assert!(version_is_newer("1.2.3", "1.2.4"));
        assert!(!version_is_newer("1.2.4", "1.2.3"));
        assert!(!version_is_newer("1.2.3", "1.2.3"));
        assert!(version_is_newer("release-a", "release-b"));
    }

    #[test]
    fn installer_rejects_registry_semver_downgrades() {
        let error = reject_semver_downgrade("2.4.0", "2.3.9").unwrap_err();
        assert_eq!(error.code, "agent_install_downgrade_rejected");
        assert!(reject_semver_downgrade("2.4.0", "2.4.0").is_ok());
        assert!(reject_semver_downgrade("2.4.0", "2.5.0").is_ok());
    }

    #[test]
    fn canonical_npm_tarball_sources_cover_scoped_packages() {
        let expected = "https://registry.npmjs.org/@agentclientprotocol/claude-agent-acp/-/claude-agent-acp-0.65.0.tgz";
        assert_eq!(
            canonical_npm_tarball_url("@agentclientprotocol/claude-agent-acp", "0.65.0").unwrap(),
            expected
        );
        assert_eq!(
            validate_canonical_npm_tarball_source(
                expected,
                "@agentclientprotocol/claude-agent-acp",
                "0.65.0"
            )
            .unwrap(),
            expected
        );
        assert!(
            validate_canonical_npm_tarball_source(
                "https://packages.example.invalid/claude-agent-acp-0.65.0.tgz",
                "@agentclientprotocol/claude-agent-acp",
                "0.65.0"
            )
            .is_err()
        );
    }

    #[test]
    fn npm_lock_requires_the_canonical_tarball_source() {
        let temp = tempfile::tempdir().unwrap();
        let package = "@scope/agent";
        let version = "1.2.3";
        let integrity = "sha512-fixture";
        let resolved = canonical_npm_tarball_url(package, version).unwrap();
        write_json_private(
            &temp.path().join("package-lock.json"),
            &serde_json::json!({
                "packages": {
                    "node_modules/@scope/agent": {
                        "version": version,
                        "integrity": integrity,
                        "resolved": resolved,
                    }
                }
            }),
        )
        .unwrap();
        assert!(verify_npm_lock(temp.path(), package, version, integrity, &resolved).is_ok());
        write_json_private(
            &temp.path().join("package-lock.json"),
            &serde_json::json!({
                "packages": {
                    "node_modules/@scope/agent": {
                        "version": version,
                        "integrity": integrity,
                        "resolved": "https://packages.example.invalid/agent-1.2.3.tgz",
                    }
                }
            }),
        )
        .unwrap();
        assert!(verify_npm_lock(temp.path(), package, version, integrity, &resolved).is_err());
    }

    #[test]
    fn npm_executable_selection_is_unambiguous() {
        let metadata = NpmPackageMetadata {
            name: "@scope/agent".to_string(),
            version: "1.0.0".to_string(),
            dist: NpmDist {
                integrity: None,
                tarball: None,
            },
            bin: Some(NpmBin::Multiple(BTreeMap::from([
                ("agent-a".to_string(), "a.js".to_string()),
                ("agent-b".to_string(), "b.js".to_string()),
            ]))),
        };
        assert!(select_npm_bin(&metadata, "@scope/agent").is_err());
    }

    #[test]
    fn invalid_cached_installations_are_removed_before_reinstall() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("broken-install");
        fs::create_dir(&root).unwrap();
        assert!(
            load_or_remove_cached_installation(&root, "fingerprint")
                .unwrap()
                .is_none()
        );
        assert!(!root.exists());
    }

    #[test]
    fn interrupted_installation_recovery_requires_usable_files() {
        let temp = tempfile::tempdir().unwrap();
        let command = temp.path().join("agent");
        fs::write(&command, b"fixture").unwrap();
        let record = AgentManagedInstallationRecord {
            agent_id: AgentId::parse("claude").unwrap(),
            registry_agent_id: "claude-acp".to_string(),
            state: AgentManagedInstallState {
                managed: true,
                status: AgentManagedInstallStatus::Installing,
                distribution_kind: Some(AgentManagedDistributionKind::Binary),
                installed_version: Some("1.0.0".to_string()),
                available_version: Some("1.0.0".to_string()),
                last_error_code: None,
                last_error_message: None,
                updated_at_ms: Some(1),
            },
            command: Some(AgentCommandConfig {
                command: command.to_string_lossy().into_owned(),
                args: Vec::new(),
            }),
            install_root: Some(temp.path().to_string_lossy().into_owned()),
            updated_at_ms: 1,
        };
        assert!(installation_files_are_usable(&record));
        assert!(!record_has_usable_installation(&record));
        fs::remove_file(command).unwrap();
        assert!(!installation_files_are_usable(&record));
    }

    #[tokio::test]
    async fn uninstall_removes_files_state_auth_and_bootstrap_membership() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("vibex.db");
        let root = temp.path().join("managed-agents");
        let config_service = ProviderConfigService::new(&db_path);
        let service = AgentInstallService::new(&db_path, &root, config_service.clone()).unwrap();
        let agent_id = AgentId::parse("claude").unwrap();
        let install_root = root.join("agents/claude/0.65.0-fixture");
        fs::create_dir_all(&install_root).unwrap();
        let command_path = install_root.join("claude-agent-acp");
        fs::write(&command_path, b"fixture").unwrap();
        let command = AgentCommandConfig {
            command: command_path.to_string_lossy().into_owned(),
            args: Vec::new(),
        };
        config_service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: Some(command.clone()),
                env: None,
                params: None,
            })
            .unwrap();
        service
            .write_record(&AgentManagedInstallationRecord {
                agent_id: agent_id.clone(),
                registry_agent_id: "claude-acp".to_string(),
                state: AgentManagedInstallState {
                    managed: true,
                    status: AgentManagedInstallStatus::Installed,
                    distribution_kind: Some(AgentManagedDistributionKind::Binary),
                    installed_version: Some("0.65.0".to_string()),
                    available_version: Some("0.65.0".to_string()),
                    last_error_code: None,
                    last_error_message: None,
                    updated_at_ms: Some(1),
                },
                command: Some(command),
                install_root: Some(install_root.to_string_lossy().into_owned()),
                updated_at_ms: 1,
            })
            .unwrap();
        let mut conn = open_database(&db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        vibex_db::AgentAuthCatalogSnapshotRepository::upsert(
            &conn,
            &vibex_db::AgentAuthCatalogSnapshotRecord {
                agent_id: agent_id.clone(),
                provider_profile_id: None,
                catalog: vibex_core::AgentAuthCatalog {
                    agent_id: agent_id.clone(),
                    methods: Vec::new(),
                    supports_logout: false,
                    status: vibex_core::AgentAuthStatus::Unknown,
                    refreshed_at_ms: 1,
                },
                refreshed_at_ms: 1,
            },
        )
        .unwrap();
        drop(conn);

        let state = service.uninstall(agent_id.clone()).await.unwrap();
        assert_eq!(state.status, AgentManagedInstallStatus::NotInstalled);
        assert!(!root.join("agents/claude").exists());
        assert!(service.read_record(&agent_id).unwrap().is_none());
        let agent = config_service
            .list_agents(AgentListRequest {
                include_disabled: true,
            })
            .unwrap()
            .agents
            .into_iter()
            .find(|agent| agent.id == agent_id)
            .unwrap();
        assert!(!agent.added);
        assert!(!agent.enabled);
        let mut conn = open_database(&db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        assert!(
            vibex_db::AgentAuthCatalogSnapshotRepository::get(&conn, &agent_id, None)
                .unwrap()
                .is_none()
        );
        assert!(!service.bootstrap_agent_ids().unwrap().contains(&agent_id));
    }

    #[test]
    fn tar_bz2_archives_are_extracted_and_unsupported_archives_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("agent.tar.bz2");
        let archive_file = File::create(&archive_path).unwrap();
        let encoder = bzip2::write::BzEncoder::new(archive_file, bzip2::Compression::best());
        let mut builder = tar::Builder::new(encoder);
        let payload = b"agent-binary";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "agent", &payload[..])
            .unwrap();
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        let destination = temp.path().join("extracted");
        fs::create_dir(&destination).unwrap();
        extract_archive(
            &archive_path,
            "https://example.com/agent.tar.bz2",
            &destination,
        )
        .unwrap();
        assert_eq!(fs::read(destination.join("agent")).unwrap(), payload);

        let error = extract_archive(
            &archive_path,
            "https://example.com/agent.tar.xz",
            &destination,
        )
        .unwrap_err();
        assert_eq!(error.code, "agent_archive_format_unsupported");
    }

    #[test]
    fn archive_budgets_and_credential_urls_are_rejected() {
        assert!(check_archive_budget(ARCHIVE_MAX_ENTRIES + 1, 0).is_err());
        assert!(check_archive_budget(1, ARCHIVE_MAX_UNPACKED_BYTES + 1).is_err());
        assert!(validate_https_url("https://:secret@example.com/agent", "Agent").is_err());
    }

    #[test]
    fn registry_cache_rejects_future_and_expired_timestamps() {
        let temp = tempfile::tempdir().unwrap();
        let metadata = temp.path().join("metadata.json");
        write_json_private(
            &metadata,
            &RegistryCacheMetadata {
                fetched_at_ms: 1_000,
            },
        )
        .unwrap();
        assert!(registry_cache_is_fresh(&metadata, 1_001));
        assert!(!registry_cache_is_fresh(&metadata, 999));
        assert!(!registry_cache_is_fresh(
            &metadata,
            1_000 + REGISTRY_CACHE_MAX_AGE_MS + 1
        ));
    }
}

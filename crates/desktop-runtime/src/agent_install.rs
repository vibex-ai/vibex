//! Verified, side-by-side ACP Agent installations owned by the desktop runtime.

use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
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
use tokio::sync::{Mutex, OwnedMutexGuard};
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
const NPM_REGISTRY_BASE_URL: &str = "https://registry.npmjs.org";
const PYPI_JSON_BASE_URL: &str = "https://pypi.org/pypi";
const KIRO_MANIFEST_URL: &str = "https://prod.download.cli.kiro.dev/stable/latest/manifest.json";
const KIRO_DOWNLOAD_BASE_URL: &str = "https://prod.download.cli.kiro.dev/stable/latest";
const REGISTRY_CACHE_MAX_AGE_MS: i64 = 60 * 60 * 1_000;
const REGISTRY_MAX_BYTES: usize = 5 * 1024 * 1024;
const DOWNLOAD_MAX_BYTES: u64 = 768 * 1024 * 1024;
const ARCHIVE_MAX_ENTRIES: usize = 100_000;
const ARCHIVE_MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const NODE_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const UV_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const MINIMUM_NODE_VERSION: semver::Version = semver::Version::new(22, 0, 0);
const MINIMUM_UV_VERSION: semver::Version = semver::Version::new(0, 5, 0);
const PI_MINIMUM_NODE_VERSION: semver::Version = semver::Version::new(22, 19, 0);
const PI_CODING_AGENT_PACKAGE: &str = "@earendil-works/pi-coding-agent";
const PI_COMMAND_NAME: &str = "pi";
const NPM_COMPANION_LAUNCHER_NAME: &str = "vibex-acp-companion-launcher.cjs";
const AMP_CLI_PACKAGE: &str = "@ampcode/cli";
const AUTOHAND_CLI_PACKAGE: &str = "autohand-cli";
const CODEWHALE_CLI_PACKAGE: &str = "codewhale";
const HERMES_CLI_PACKAGE: &str = "hermes-agent";
const NODE_RELEASE_INDEX_URL: &str = "https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt";
const UV_RELEASE_DOWNLOAD_BASE: &str = "https://github.com/astral-sh/uv/releases/latest/download";
const AGENT_NODE_PATH_ENV: &str = "VIBEX_AGENT_NODE_PATH";
const AGENT_NPM_PATH_ENV: &str = "VIBEX_AGENT_NPM_PATH";
const AGENT_UV_PATH_ENV: &str = "VIBEX_AGENT_UV_PATH";
const UV_MANAGED_PYTHON_REQUEST: &str = "3.12";
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentUvRuntimeOptions {
    pub uv_path: Option<PathBuf>,
}

impl AgentUvRuntimeOptions {
    pub fn from_environment() -> Self {
        Self {
            uv_path: nonempty_environment_path(AGENT_UV_PATH_ENV),
        }
    }
}

#[derive(Clone)]
pub struct AgentInstallService {
    db_path: PathBuf,
    root: PathBuf,
    config_service: ProviderConfigService,
    node_runtime_options: AgentNodeRuntimeOptions,
    uv_runtime_options: AgentUvRuntimeOptions,
    client: Client,
    operation_locks: Arc<Mutex<BTreeMap<String, Weak<Mutex<()>>>>>,
}

impl AgentInstallService {
    pub fn new(
        db_path: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
        config_service: ProviderConfigService,
    ) -> VibexResult<Self> {
        Self::new_with_runtime_options(
            db_path,
            root,
            config_service,
            AgentNodeRuntimeOptions::default(),
            AgentUvRuntimeOptions::default(),
        )
    }

    pub fn new_with_node_runtime_options(
        db_path: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
        config_service: ProviderConfigService,
        node_runtime_options: AgentNodeRuntimeOptions,
    ) -> VibexResult<Self> {
        Self::new_with_runtime_options(
            db_path,
            root,
            config_service,
            node_runtime_options,
            AgentUvRuntimeOptions::from_environment(),
        )
    }

    pub fn new_with_runtime_options(
        db_path: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
        config_service: ProviderConfigService,
        node_runtime_options: AgentNodeRuntimeOptions,
        uv_runtime_options: AgentUvRuntimeOptions,
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
            uv_runtime_options,
            client,
            operation_locks: Arc::new(Mutex::new(BTreeMap::new())),
        };
        service.recover_interrupted_operations()?;
        Ok(service)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn install(&self, agent_id: AgentId) -> VibexResult<AgentManagedInstallState> {
        let _guard = self.acquire_agent_operation(&agent_id).await;
        self.install_locked(agent_id).await
    }

    /// Restores an existing healthy installation during startup without
    /// silently upgrading it. A missing or invalid installation is repaired.
    pub async fn ensure_installed(
        &self,
        agent_id: AgentId,
    ) -> VibexResult<AgentManagedInstallState> {
        let _guard = self.acquire_agent_operation(&agent_id).await;
        if let Some(record) = self.read_record(&agent_id)?
            && record_has_usable_installation(&record)
            && managed_companion_installation_is_usable(&agent_id, &record)
        {
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
        let _guard = self.acquire_agent_operation(&agent_id).await;
        let (registry_id, entry) = self.load_install_entry(&agent_id, true).await?;
        let distribution = resolve_distribution(&entry)?;
        let now = unix_timestamp_ms();
        let existing = self.read_record(&agent_id)?;
        let distribution_kind = distribution.kind();
        let existing_is_usable = existing.as_ref().is_some_and(|record| {
            record_has_usable_installation(record)
                && record_matches_distribution(record, distribution_kind)
        });
        let installed_version = existing_is_usable
            .then(|| {
                existing
                    .as_ref()
                    .and_then(|record| record.state.installed_version.clone())
            })
            .flatten();
        let runtime_update = existing_is_usable
            && self
                .managed_runtime_version_is_newer(&agent_id, existing.as_ref())
                .await?;
        let status = match installed_version.as_deref() {
            Some(version) if version_is_newer(version, &entry.version) => {
                AgentManagedInstallStatus::UpdateAvailable
            }
            Some(_) if runtime_update => AgentManagedInstallStatus::UpdateAvailable,
            Some(_) => AgentManagedInstallStatus::Installed,
            None => AgentManagedInstallStatus::NotInstalled,
        };
        let state = AgentManagedInstallState {
            managed: true,
            status,
            distribution_kind: Some(distribution_kind),
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
        let _guard = self.acquire_agent_operation(&agent_id).await;
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

    async fn acquire_agent_operation(&self, agent_id: &AgentId) -> OwnedMutexGuard<()> {
        self.acquire_operation(format!("agent:{}", agent_id.as_str()))
            .await
    }

    async fn acquire_operation(&self, key: String) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.operation_locks.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(Mutex::new(()));
                locks.insert(key, Arc::downgrade(&lock));
                lock
            }
        };
        lock.lock_owned().await
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
        let (registry_id, entry) = self.load_install_entry(&agent_id, false).await?;
        let distribution = resolve_distribution(&entry)?;
        let distribution_kind = distribution.kind();
        let (node_runtime, uv_runtime) = match &distribution {
            ResolvedDistribution::Npm(_) => (
                Some(
                    self.select_node_runtime(&minimum_node_version(&agent_id))
                        .await?,
                ),
                None,
            ),
            ResolvedDistribution::Uvx(_) => (None, Some(self.select_uv_runtime().await?)),
            ResolvedDistribution::Binary(_) if latest_npm_companion(&agent_id).is_some() => (
                Some(
                    self.select_node_runtime(&minimum_node_version(&agent_id))
                        .await?,
                ),
                None,
            ),
            ResolvedDistribution::Binary(_) => (None, None),
            ResolvedDistribution::Kiro(_) => (None, None),
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
            record_matches_distribution(record, distribution_kind)
                && record.state.installed_version.as_deref() == Some(entry.version.as_str())
                && record.install_root.as_deref().is_some_and(|root| {
                    Path::new(root).is_dir()
                        && installed_distribution_kind(Path::new(root)) == Some(distribution_kind)
                })
                && record.command.as_ref().is_some_and(command_is_available)
                && latest_npm_companion(&agent_id).is_none()
                && node_runtime.as_ref().is_none_or(|runtime| {
                    record.command.as_ref().is_some_and(|command| {
                        Path::new(&command.command) == runtime.node.as_path()
                    })
                })
        }) {
            let mut record = previous.expect("matching managed record exists");
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
            .install_distribution(&agent_id, &entry, distribution, node_runtime, uv_runtime)
            .await;

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
        uv_runtime: Option<UvRuntime>,
    ) -> VibexResult<InstalledAgent> {
        match distribution {
            ResolvedDistribution::Binary(target) => {
                self.install_binary(agent_id, entry, target, node_runtime)
                    .await
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
            ResolvedDistribution::Uvx(uvx) => {
                let uv_runtime = uv_runtime.ok_or_else(|| {
                    VibexError::capability(
                        "agent_uv_runtime_unselected",
                        "uvx Agent installation has no selected uv runtime",
                    )
                })?;
                self.install_uvx(agent_id, entry, uvx, uv_runtime).await
            }
            ResolvedDistribution::Kiro(kiro) => self.install_kiro(agent_id, entry, kiro).await,
        }
    }

    async fn load_install_entry(
        &self,
        agent_id: &AgentId,
        force_registry: bool,
    ) -> VibexResult<(String, RegistryEntry)> {
        if let Some(custom) = ManagedCliAgent::for_agent(agent_id) {
            return self.load_latest_cli_entry(custom).await;
        }
        let registry_id = require_registry_id(agent_id)?;
        let registry = self.load_registry(force_registry).await?;
        match registry.require_agent(registry_id) {
            Ok(entry) => Ok((registry_id.to_string(), entry.clone())),
            Err(_error) if agent_id.as_str() == "amp-acp" => {
                let entry = self
                    .load_latest_npm_entry("amp-acp", "amp-acp", Vec::new())
                    .await?;
                Ok((registry_id.to_string(), entry))
            }
            Err(error) => Err(error),
        }
    }

    async fn load_latest_cli_entry(
        &self,
        agent: ManagedCliAgent,
    ) -> VibexResult<(String, RegistryEntry)> {
        let entry = match agent {
            ManagedCliAgent::Codewhale => {
                self.load_latest_npm_entry(
                    agent.registry_id(),
                    CODEWHALE_CLI_PACKAGE,
                    vec!["serve".to_string(), "--acp".to_string()],
                )
                .await?
            }
            ManagedCliAgent::Hermes => {
                let version = self.fetch_latest_pypi_version(HERMES_CLI_PACKAGE).await?;
                RegistryEntry {
                    id: agent.registry_id().to_string(),
                    version: version.clone(),
                    distribution: RegistryDistribution {
                        binary: None,
                        npx: None,
                        uvx: Some(RegistryUvxDistribution {
                            package: format!("{HERMES_CLI_PACKAGE}[acp]=={version}"),
                            args: vec!["acp".to_string()],
                        }),
                        kiro: None,
                    },
                }
            }
            ManagedCliAgent::Kiro => {
                let distribution = self.fetch_latest_kiro_distribution().await?;
                RegistryEntry {
                    id: agent.registry_id().to_string(),
                    version: distribution.version.clone(),
                    distribution: RegistryDistribution {
                        binary: None,
                        npx: None,
                        uvx: None,
                        kiro: Some(distribution),
                    },
                }
            }
        };
        Ok((agent.registry_id().to_string(), entry))
    }

    async fn load_latest_npm_entry(
        &self,
        id: &str,
        package: &str,
        args: Vec<String>,
    ) -> VibexResult<RegistryEntry> {
        let metadata = self.fetch_latest_npm_metadata(package).await?;
        let version = metadata.version;
        Ok(RegistryEntry {
            id: id.to_string(),
            version: version.clone(),
            distribution: RegistryDistribution {
                binary: None,
                npx: Some(RegistryNpxDistribution {
                    package: format!("{package}@{version}"),
                    args,
                }),
                uvx: None,
                kiro: None,
            },
        })
    }

    async fn managed_runtime_version_is_newer(
        &self,
        agent_id: &AgentId,
        existing: Option<&AgentManagedInstallationRecord>,
    ) -> VibexResult<bool> {
        let Some(record) = existing else {
            return Ok(false);
        };
        let Some(root) = record.install_root.as_deref() else {
            return Ok(false);
        };
        let Some(runtime_package) = latest_npm_companion(agent_id) else {
            return Ok(false);
        };
        let latest = self
            .fetch_latest_npm_metadata(runtime_package)
            .await?
            .version;
        let installed = read_manifest_runtime_version(Path::new(root));
        Ok(installed.is_none_or(|installed| version_is_newer(&installed, &latest)))
    }

    async fn install_binary(
        &self,
        agent_id: &AgentId,
        entry: &RegistryEntry,
        target: RegistryBinaryTarget,
        node: Option<NodeRuntime>,
    ) -> VibexResult<InstalledAgent> {
        validate_https_url(&target.archive, "Agent binary")?;
        let sha256 = optional_sha256(target.sha256.as_deref())?;
        let command_rel = safe_relative_path(&target.cmd, "binary command")?;
        let runtime_package = if let Some(package) = latest_npm_companion(agent_id) {
            let metadata = self.fetch_latest_npm_metadata(package).await?;
            Some(
                self.resolve_verified_npm_package(package, &metadata.version)
                    .await?,
            )
        } else {
            None
        };
        let node = runtime_package.is_some().then_some(node).flatten();
        if runtime_package.is_some() && node.is_none() {
            return Err(VibexError::capability(
                "agent_node_runtime_unselected",
                "npm Agent installation has no selected Node.js runtime",
            ));
        }
        let companion_launcher = runtime_package
            .as_ref()
            .map(|_| npm_companion_binary_launcher_source(agent_id, &command_rel))
            .transpose()?;
        let args_identity = serde_json::to_string(&target.args).map_err(|error| {
            VibexError::validation(
                "agent_binary_args_invalid",
                "ACP Registry binary arguments could not be verified",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let node_identity = node.as_ref().map(NodeRuntime::fingerprint_identity);
        let mut fingerprint_parts = vec![
            entry.id.as_str(),
            entry.version.as_str(),
            target.archive.as_str(),
            sha256.as_deref().unwrap_or_default(),
            target.cmd.as_str(),
            args_identity.as_str(),
        ];
        if let (Some(runtime_package), Some(node_identity)) =
            (runtime_package.as_ref(), node_identity.as_deref())
        {
            fingerprint_parts.extend([
                runtime_package.name.as_str(),
                runtime_package.version.as_str(),
                runtime_package.integrity.as_str(),
                runtime_package.tarball.as_str(),
                runtime_package.bin_path.as_str(),
                npm_companion_command(agent_id).unwrap_or_default(),
                node_identity,
            ]);
        }
        if let Some(companion_launcher) = companion_launcher.as_deref() {
            fingerprint_parts.push(companion_launcher);
        }
        let fingerprint = distribution_fingerprint(&fingerprint_parts);
        let target_root = self.version_root(agent_id, &entry.version, &fingerprint)?;
        if let Some(installed) = load_or_remove_cached_installation(&target_root, &fingerprint)? {
            return Ok(installed);
        }

        let archive = self
            .download_verified(&target.archive, sha256.as_deref())
            .await?;
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

        let command_path = staging.join(&command_rel);
        ensure_regular_file(&staging, &command_path, "agent_binary_missing")?;
        make_executable(&command_path)?;
        let launch = if let (Some(runtime_package), Some(companion_launcher), Some(node)) = (
            runtime_package.as_ref(),
            companion_launcher.as_deref(),
            node.as_ref(),
        ) {
            self.install_verified_npm_packages(agent_id, &staging, node, &[runtime_package])
                .await?;
            let runtime_script = staging.join(npm_package_bin_relative_path(runtime_package)?);
            ensure_regular_file(&staging, &runtime_script, "agent_npm_runtime_bin_missing")?;
            let runtime_command = npm_command_path(
                &staging,
                npm_companion_command(agent_id).ok_or_else(|| {
                    VibexError::validation(
                        "agent_npm_runtime_command_missing",
                        "managed npm companion command was not configured",
                    )
                })?,
            );
            ensure_regular_file(
                &staging,
                &runtime_command,
                "agent_npm_runtime_command_missing",
            )?;
            let launcher = staging.join(NPM_COMPANION_LAUNCHER_NAME);
            write_private_file(&launcher, companion_launcher.as_bytes())?;
            ManifestLaunch::Node {
                node: node.node.to_string_lossy().into_owned(),
                script: NPM_COMPANION_LAUNCHER_NAME.to_string(),
                args: target.args,
            }
        } else {
            ManifestLaunch::Binary {
                command: command_rel.to_string_lossy().into_owned(),
                args: target.args,
            }
        };
        let manifest = InstallManifest {
            registry_agent_id: entry.id.clone(),
            version: entry.version.clone(),
            fingerprint: fingerprint.clone(),
            runtime_version: runtime_package.map(|package| package.version),
            distribution_kind: AgentManagedDistributionKind::Binary,
            launch,
        };
        write_json_private(&staging.join("vibex-install.json"), &manifest)?;
        publish_staging(&staging, &target_root)?;
        staging_guard.disarm();
        load_installed_agent(&target_root, &fingerprint)
    }

    async fn install_kiro(
        &self,
        agent_id: &AgentId,
        entry: &RegistryEntry,
        kiro: RegistryKiroDistribution,
    ) -> VibexResult<InstalledAgent> {
        validate_https_url(&kiro.archive, "Kiro CLI archive")?;
        let fingerprint = distribution_fingerprint(&[
            entry.id.as_str(),
            entry.version.as_str(),
            kiro.archive.as_str(),
            kiro.sha256.as_str(),
            kiro.file_type.as_str(),
            kiro.cli_path.as_deref().unwrap_or_default(),
        ]);
        let target_root = self.version_root(agent_id, &entry.version, &fingerprint)?;
        if let Some(installed) = load_or_remove_cached_installation(&target_root, &fingerprint)? {
            return Ok(installed);
        }

        let archive = self
            .download_verified(&kiro.archive, Some(kiro.sha256.as_str()))
            .await?;
        let staging = self.create_staging(agent_id)?;
        let mut staging_guard = StagingGuard::new(staging.clone());

        #[cfg(target_os = "linux")]
        let command_path = {
            if kiro.file_type != "tarGz" {
                return Err(VibexError::capability(
                    "agent_kiro_archive_unsupported",
                    "Kiro CLI Linux distribution is not a supported tar.gz archive",
                ));
            }
            let archive_url = kiro.archive.clone();
            let archive_for_extract = archive.clone();
            let staging_for_extract = staging.clone();
            tokio::task::spawn_blocking(move || {
                extract_archive(&archive_for_extract, &archive_url, &staging_for_extract)
            })
            .await
            .map_err(|error| {
                VibexError::process(
                    "agent_archive_extract_join_failed",
                    "Kiro CLI archive extraction task failed",
                )
                .with_diagnostic("error", error.to_string())
            })??;

            let package_root = staging.join("kirocli");
            let install_script = package_root.join("install.sh");
            ensure_regular_file(
                &staging,
                &install_script,
                "agent_kiro_install_script_missing",
            )?;
            make_executable(&install_script)?;
            let private_home = staging.join("home");
            fs::create_dir_all(&private_home).map_err(|error| {
                storage_error(
                    "agent_kiro_home_create_failed",
                    "Kiro CLI private home could not be created",
                    error,
                )
            })?;
            let mut command = Command::new(&install_script);
            command
                .current_dir(&package_root)
                .env("HOME", &private_home)
                .env("XDG_CONFIG_HOME", private_home.join(".config"))
                .env("XDG_DATA_HOME", private_home.join(".local/share"))
                .env("XDG_CACHE_HOME", private_home.join(".cache"))
                .env("KIRO_CLI_SKIP_SETUP", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            run_install_command(
                command,
                "agent_kiro_setup_timeout",
                "agent_kiro_setup_spawn_failed",
                "agent_kiro_setup_failed",
                "Kiro CLI private installation failed",
            )
            .await?;
            private_home.join(".local/bin/kiro-cli")
        };

        #[cfg(target_os = "macos")]
        let command_path = install_kiro_macos(&archive, &staging, kiro.cli_path.as_deref()).await?;

        #[cfg(target_os = "windows")]
        let command_path = install_kiro_windows(&archive, &staging).await?;

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let command_path = {
            let _ = (&archive, &staging, &kiro);
            return Err(VibexError::capability(
                "agent_kiro_platform_unsupported",
                "Kiro CLI is not supported on this platform",
            ));
        };

        ensure_regular_file(&staging, &command_path, "agent_kiro_binary_missing")?;
        make_executable(&command_path)?;
        let command_rel = command_path.strip_prefix(&staging).map_err(|_| {
            VibexError::validation(
                "agent_kiro_binary_outside_install",
                "Kiro CLI executable escaped the managed installation",
            )
        })?;
        let manifest = InstallManifest {
            registry_agent_id: entry.id.clone(),
            version: entry.version.clone(),
            fingerprint: fingerprint.clone(),
            runtime_version: Some(kiro.version),
            distribution_kind: AgentManagedDistributionKind::Binary,
            launch: ManifestLaunch::Binary {
                command: command_rel.to_string_lossy().into_owned(),
                args: vec!["acp".to_string()],
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
        let package = self
            .resolve_verified_npm_package(package, package_version)
            .await?;
        let runtime_package = if let Some(package) = latest_npm_companion(agent_id) {
            let metadata = self.fetch_latest_npm_metadata(package).await?;
            Some(
                self.resolve_verified_npm_package(package, &metadata.version)
                    .await?,
            )
        } else {
            None
        };
        let adapter_script_rel = npm_package_bin_relative_path(&package)?;
        let companion_launcher = runtime_package
            .as_ref()
            .map(|_| npm_companion_launcher_source(agent_id, &adapter_script_rel))
            .transpose()?;
        let args_identity = serde_json::to_string(&npx.args).map_err(|error| {
            VibexError::validation(
                "agent_npm_args_invalid",
                "ACP Registry npm arguments could not be verified",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let node_identity = node.fingerprint_identity();
        let mut fingerprint_parts = vec![
            entry.id.as_str(),
            entry.version.as_str(),
            npx.package.as_str(),
            package.integrity.as_str(),
            package.tarball.as_str(),
            package.bin_path.as_str(),
            args_identity.as_str(),
            node_identity.as_str(),
        ];
        if let Some(runtime_package) = runtime_package.as_ref() {
            fingerprint_parts.extend([
                runtime_package.name.as_str(),
                runtime_package.version.as_str(),
                runtime_package.integrity.as_str(),
                runtime_package.tarball.as_str(),
                runtime_package.bin_path.as_str(),
                npm_companion_command(agent_id).unwrap_or_default(),
            ]);
        }
        if let Some(companion_launcher) = companion_launcher.as_deref() {
            fingerprint_parts.push(companion_launcher);
        }
        let fingerprint = distribution_fingerprint(&fingerprint_parts);
        let target_root = self.version_root(agent_id, &entry.version, &fingerprint)?;
        if let Some(installed) = load_or_remove_cached_installation(&target_root, &fingerprint)? {
            return Ok(installed);
        }

        let staging = self.create_staging(agent_id)?;
        let mut staging_guard = StagingGuard::new(staging.clone());
        let mut packages = vec![&package];
        if let Some(runtime_package) = runtime_package.as_ref() {
            packages.push(runtime_package);
        }
        self.install_verified_npm_packages(agent_id, &staging, &node, &packages)
            .await?;
        let adapter_script = staging.join(&adapter_script_rel);
        ensure_regular_file(&staging, &adapter_script, "agent_npm_bin_missing")?;
        let script = if let (Some(runtime_package), Some(companion_launcher)) =
            (runtime_package.as_ref(), companion_launcher.as_deref())
        {
            let runtime_script = staging.join(npm_package_bin_relative_path(runtime_package)?);
            ensure_regular_file(&staging, &runtime_script, "agent_npm_runtime_bin_missing")?;
            let runtime_command = npm_command_path(
                &staging,
                npm_companion_command(agent_id).ok_or_else(|| {
                    VibexError::validation(
                        "agent_npm_runtime_command_missing",
                        "managed npm companion command was not configured",
                    )
                })?,
            );
            ensure_regular_file(
                &staging,
                &runtime_command,
                "agent_npm_runtime_command_missing",
            )?;
            let launcher = staging.join(NPM_COMPANION_LAUNCHER_NAME);
            write_private_file(&launcher, companion_launcher.as_bytes())?;
            launcher
        } else {
            adapter_script
        };
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
            runtime_version: runtime_package
                .as_ref()
                .map(|package| package.version.clone())
                .or_else(|| (agent_id.as_str() == "codewhale").then(|| package.version.clone())),
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

    async fn install_verified_npm_packages(
        &self,
        agent_id: &AgentId,
        staging: &Path,
        node: &NodeRuntime,
        packages: &[&VerifiedNpmPackage],
    ) -> VibexResult<()> {
        if packages.is_empty() {
            return Err(VibexError::validation(
                "agent_npm_packages_missing",
                "managed npm installation did not specify any packages",
            ));
        }
        let dependencies =
            packages
                .iter()
                .fold(serde_json::Map::new(), |mut dependencies, package| {
                    dependencies.insert(
                        package.name.clone(),
                        serde_json::Value::String(format!("={}", package.version)),
                    );
                    dependencies
                });
        let package_json = serde_json::json!({
            "name": "vibex-managed-acp-agent",
            "private": true,
            "version": "0.0.0",
            "dependencies": dependencies,
        });
        write_json_private(&staging.join("package.json"), &package_json)?;
        let npm_config = write_isolated_npm_configs(staging)?;
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
            .arg("--");
        for package in packages {
            command.arg(package.exact_spec());
        }
        command
            .current_dir(staging)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .env("npm_config_cache", self.root.join("cache/npm"))
            .env("npm_config_userconfig", &npm_config.user)
            .env("npm_config_globalconfig", &npm_config.global)
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

        run_trusted_npm_setup(agent_id, staging, node, &npm_config).await?;
        for package in packages {
            verify_npm_lock(
                staging,
                &package.name,
                &package.version,
                &package.integrity,
                &package.tarball,
            )?;
        }
        Ok(())
    }

    async fn install_uvx(
        &self,
        agent_id: &AgentId,
        entry: &RegistryEntry,
        uvx: RegistryUvxDistribution,
        uv: UvRuntime,
    ) -> VibexResult<InstalledAgent> {
        let package = parse_exact_uvx_spec(&uvx.package, &entry.version)?;
        let entry_point_identity = managed_uvx_entry_point(agent_id).unwrap_or(&package.name);
        let args_identity = serde_json::to_string(&uvx.args).map_err(|error| {
            VibexError::validation(
                "agent_uvx_args_invalid",
                "ACP Registry uvx arguments could not be verified",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let uv_identity = uv.fingerprint_identity();
        let fingerprint = distribution_fingerprint(&[
            entry.id.as_str(),
            entry.version.as_str(),
            uvx.package.as_str(),
            package.name.as_str(),
            package.version.as_str(),
            entry_point_identity,
            args_identity.as_str(),
            uv_identity.as_str(),
        ]);
        let target_root = self.version_root(agent_id, &entry.version, &fingerprint)?;
        if let Some(installed) = load_or_remove_cached_installation(&target_root, &fingerprint)? {
            return Ok(installed);
        }

        let staging = self.create_staging(agent_id)?;
        let mut staging_guard = StagingGuard::new(staging.clone());
        let cache_dir = self.root.join("cache/uv");
        let python_dir = self.root.join("runtimes/python");
        ensure_uv_cache_directories(&cache_dir, &python_dir)?;

        let venv = staging.join("venv");
        let mut venv_command = uv.command();
        configure_isolated_uv_command(&mut venv_command, &cache_dir, &python_dir);
        venv_command
            .arg("venv")
            .arg("--no-config")
            .arg("--managed-python")
            .arg("--python")
            .arg(UV_MANAGED_PYTHON_REQUEST)
            .arg("--relocatable")
            .arg(&venv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        run_install_command(
            venv_command,
            "agent_uvx_venv_timeout",
            "agent_uvx_venv_spawn_failed",
            "agent_uvx_venv_failed",
            "managed uvx virtual environment could not be created",
        )
        .await?;

        let python = uv_venv_python(&venv);
        ensure_managed_uv_python(&self.root, &staging, &python)?;
        let mut install_command = uv.command();
        configure_isolated_uv_command(&mut install_command, &cache_dir, &python_dir);
        install_command
            .arg("pip")
            .arg("install")
            .arg("--no-config")
            .arg("--python")
            .arg(&python)
            .arg("--default-index")
            .arg("https://pypi.org/simple")
            .arg("--keyring-provider")
            .arg("disabled")
            .arg("--link-mode")
            .arg("copy")
            .arg(package.exact_spec())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        run_install_command(
            install_command,
            "agent_uvx_install_timeout",
            "agent_uvx_install_spawn_failed",
            "agent_uvx_install_failed",
            "managed uvx installation failed",
        )
        .await?;

        let installed_package = inspect_uvx_package(&python, &package.name).await?;
        if installed_package.version != package.version {
            return Err(VibexError::validation(
                "agent_uvx_package_version_mismatch",
                "uvx installed package version did not match the ACP Registry",
            )
            .with_diagnostic("package", package.name)
            .with_diagnostic("expectedVersion", package.version)
            .with_diagnostic("actualVersion", installed_package.version));
        }
        let executable = select_uvx_entry_point(entry_point_identity, &installed_package.scripts)?;
        let launcher = staging.join("vibex-uvx-launcher.py");
        let launcher_source = uvx_launcher_source(&package.name, &executable)?;
        write_private_file(&launcher, launcher_source.as_bytes())?;
        ensure_regular_file(&staging, &launcher, "agent_uvx_launcher_missing")?;

        let python_rel = python.strip_prefix(&staging).map_err(|_| {
            VibexError::validation(
                "agent_uvx_python_outside_install",
                "uvx virtual environment escaped the managed installation",
            )
        })?;
        let launcher_rel = launcher.strip_prefix(&staging).map_err(|_| {
            VibexError::validation(
                "agent_uvx_launcher_outside_install",
                "uvx launcher escaped the managed installation",
            )
        })?;
        let manifest = InstallManifest {
            registry_agent_id: entry.id.clone(),
            version: entry.version.clone(),
            fingerprint: fingerprint.clone(),
            runtime_version: Some(package.version.clone()),
            distribution_kind: AgentManagedDistributionKind::Uvx,
            launch: ManifestLaunch::Python {
                python: python_rel.to_string_lossy().into_owned(),
                script: launcher_rel.to_string_lossy().into_owned(),
                args: uvx.args,
            },
        };
        write_json_private(&staging.join("vibex-install.json"), &manifest)?;
        publish_staging(&staging, &target_root)?;
        staging_guard.disarm();
        load_installed_agent(&target_root, &fingerprint)
    }

    async fn load_registry(&self, force: bool) -> VibexResult<RegistryIndex> {
        let requested_at_ms = unix_timestamp_ms();
        let _guard = self.acquire_operation("shared:registry".into()).await;
        let cache_path = self.root.join("registry/registry.json");
        let metadata_path = self.root.join("registry/metadata.json");
        let cached = fs::read(&cache_path).ok();
        let refreshed_while_waiting = force
            && cached.is_some()
            && registry_cache_fetched_at_ms(&metadata_path)
                .is_some_and(|fetched_at_ms| fetched_at_ms >= requested_at_ms);
        if cached.is_some()
            && ((!force && registry_cache_is_fresh(&metadata_path, unix_timestamp_ms()))
                || refreshed_while_waiting)
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

    async fn fetch_latest_npm_metadata(&self, package: &str) -> VibexResult<NpmPackageMetadata> {
        let encoded = url::form_urlencoded::byte_serialize(package.as_bytes()).collect::<String>();
        let url = format!("{NPM_REGISTRY_BASE_URL}/{encoded}/latest");
        let bytes = self.fetch_limited(&url, REGISTRY_MAX_BYTES).await?;
        let metadata: NpmPackageMetadata = serde_json::from_slice(&bytes).map_err(|error| {
            VibexError::validation(
                "agent_npm_metadata_invalid",
                "latest npm package metadata was invalid",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        if metadata.name != package {
            return Err(VibexError::validation(
                "agent_npm_metadata_identity_mismatch",
                "latest npm package metadata did not match the requested package",
            ));
        }
        validate_npm_version(&metadata.version, "latest npm package version")?;
        validate_canonical_npm_tarball_source(
            metadata.dist.tarball.as_deref().ok_or_else(|| {
                VibexError::validation(
                    "agent_npm_tarball_missing",
                    "latest npm package metadata has no tarball source",
                )
            })?,
            package,
            &metadata.version,
        )?;
        Ok(metadata)
    }

    async fn fetch_latest_pypi_version(&self, package: &str) -> VibexResult<String> {
        let url = format!("{PYPI_JSON_BASE_URL}/{package}/json");
        let bytes = self.fetch_limited(&url, REGISTRY_MAX_BYTES).await?;
        let metadata: PypiPackageMetadata = serde_json::from_slice(&bytes).map_err(|error| {
            VibexError::validation(
                "agent_pypi_metadata_invalid",
                "latest PyPI package metadata was invalid",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        if metadata.info.name != package {
            return Err(VibexError::validation(
                "agent_pypi_metadata_identity_mismatch",
                "latest PyPI package metadata did not match the requested package",
            ));
        }
        validate_npm_version(&metadata.info.version, "latest PyPI package version")?;
        Ok(metadata.info.version)
    }

    async fn fetch_latest_kiro_distribution(&self) -> VibexResult<RegistryKiroDistribution> {
        let bytes = self
            .fetch_limited(KIRO_MANIFEST_URL, REGISTRY_MAX_BYTES)
            .await?;
        let manifest: KiroReleaseManifest = serde_json::from_slice(&bytes).map_err(|error| {
            VibexError::validation(
                "agent_kiro_manifest_invalid",
                "latest Kiro CLI manifest was invalid",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        validate_npm_version(&manifest.version, "latest Kiro CLI version")?;
        let package = select_kiro_package(&manifest.packages)?;
        let archive = kiro_archive_url(&package.download, &manifest.version)?;
        validate_https_url(&archive, "Kiro CLI archive")?;
        Ok(RegistryKiroDistribution {
            archive,
            sha256: validate_sha256(&package.sha256)?,
            cli_path: package.cli_path,
            file_type: package.file_type,
            version: manifest.version,
        })
    }

    async fn resolve_verified_npm_package(
        &self,
        package: &str,
        version: &str,
    ) -> VibexResult<VerifiedNpmPackage> {
        let metadata = self.fetch_npm_metadata(package, version).await?;
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
            version,
        )?;
        let bin_path = select_npm_bin(&metadata, package)?;
        Ok(VerifiedNpmPackage {
            name: package.to_string(),
            version: version.to_string(),
            integrity: integrity.to_string(),
            tarball,
            bin_path,
        })
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

    async fn download_verified(
        &self,
        url: &str,
        expected_sha256: Option<&str>,
    ) -> VibexResult<PathBuf> {
        validate_https_url(url, "Agent archive")?;
        let cache_dir = self.root.join("cache/downloads");
        fs::create_dir_all(&cache_dir).map_err(|error| {
            storage_error(
                "agent_download_cache_create_failed",
                "Agent download cache could not be created",
                error,
            )
        })?;
        let cache_key = expected_sha256
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("url-{}", distribution_fingerprint(&[url])));
        let _guard = self
            .acquire_operation(format!("shared:download:{cache_key}"))
            .await;
        let cached = cache_dir.join(cache_key);
        if cached.is_file()
            && expected_sha256
                .is_none_or(|expected| sha256_file(&cached).is_ok_and(|actual| actual == expected))
        {
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
        if expected_sha256.is_some_and(|expected| actual != expected) {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(VibexError::validation(
                "agent_download_checksum_mismatch",
                "Agent archive SHA-256 did not match the Registry",
            )
            .with_diagnostic(
                "expectedSha256",
                expected_sha256.unwrap_or_default().to_string(),
            )
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

    async fn select_node_runtime(
        &self,
        minimum_version: &semver::Version,
    ) -> VibexResult<NodeRuntime> {
        if let Some(runtime) = self.select_external_node_runtime(minimum_version).await {
            return Ok(runtime);
        }
        let _guard = self.acquire_operation("shared:runtime:node".into()).await;
        let runtime = self.ensure_managed_node_runtime().await?;
        validate_minimum_node_version(&runtime.version, minimum_version)?;
        Ok(runtime)
    }

    async fn select_external_node_runtime(
        &self,
        minimum_version: &semver::Version,
    ) -> Option<NodeRuntime> {
        select_valid_external_node_runtime(self.node_runtime_candidates(), minimum_version).await
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
        let archive = self.download_verified(&url, Some(&sha256)).await?;
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

    async fn select_uv_runtime(&self) -> VibexResult<UvRuntime> {
        if let Some(runtime) = self.select_external_uv_runtime().await {
            return Ok(runtime);
        }
        let _guard = self.acquire_operation("shared:runtime:uv".into()).await;
        self.ensure_managed_uv_runtime().await
    }

    async fn select_external_uv_runtime(&self) -> Option<UvRuntime> {
        select_valid_external_uv_runtime(self.uv_runtime_candidates()).await
    }

    fn uv_runtime_candidates(&self) -> Vec<UvRuntimeCandidate> {
        uv_runtime_candidates(&self.uv_runtime_options, resolve_system_binary("uv"))
    }

    async fn ensure_managed_uv_runtime(&self) -> VibexResult<UvRuntime> {
        let filename = uv_archive_filename()?;
        let checksum_url = format!("{UV_RELEASE_DOWNLOAD_BASE}/{filename}.sha256");
        let checksum = parse_uv_release_checksum(
            &self.fetch_limited(&checksum_url, 16 * 1024).await?,
            filename,
        )?;
        let archive_url = format!("{UV_RELEASE_DOWNLOAD_BASE}/{filename}");
        let archive = self
            .download_verified(&archive_url, Some(&checksum))
            .await?;
        let staging_parent = self.root.join("runtimes/uv/.staging");
        fs::create_dir_all(&staging_parent).map_err(|error| {
            storage_error(
                "agent_uv_staging_create_failed",
                "uv staging directory could not be created",
                error,
            )
        })?;
        let staging = staging_parent.join(format!(
            "{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&staging).map_err(|error| {
            storage_error(
                "agent_uv_staging_create_failed",
                "uv staging directory could not be created",
                error,
            )
        })?;
        let mut staging_guard = StagingGuard::new(staging.clone());
        let archive_for_extract = archive.clone();
        let archive_url_for_extract = archive_url.clone();
        let staging_for_extract = staging.clone();
        tokio::task::spawn_blocking(move || {
            extract_archive(
                &archive_for_extract,
                &archive_url_for_extract,
                &staging_for_extract,
            )
        })
        .await
        .map_err(|error| {
            VibexError::process("agent_uv_extract_join_failed", "uv extraction task failed")
                .with_diagnostic("error", error.to_string())
        })??;

        let archive_root = staging.join(strip_archive_suffix(filename));
        if !archive_root.is_dir() {
            return Err(VibexError::validation(
                "agent_uv_archive_layout_invalid",
                "uv archive did not contain its expected root directory",
            ));
        }
        let staging_runtime = validate_uv_runtime_candidate(UvRuntimeCandidate {
            source: UvRuntimeSource::Managed,
            uv: uv_binary_at(&archive_root),
        })
        .await?;
        let target = self
            .root
            .join("runtimes/uv")
            .join(staging_runtime.version.to_string())
            .join(current_platform_key()?);
        if let Ok(runtime) = validate_uv_runtime_candidate(UvRuntimeCandidate {
            source: UvRuntimeSource::Managed,
            uv: uv_binary_at(&target),
        })
        .await
        {
            return Ok(runtime);
        }
        if fs::symlink_metadata(&target).is_ok() {
            remove_path(&target)
                .map_err(|error| error.with_diagnostic("cacheKind", "uv-runtime"))?;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                storage_error(
                    "agent_uv_runtime_parent_failed",
                    "uv runtime directory could not be created",
                    error,
                )
            })?;
        }
        fs::rename(&archive_root, &target).map_err(|error| {
            storage_error(
                "agent_uv_publish_failed",
                "verified uv runtime could not be published",
                error,
            )
        })?;
        staging_guard.disarm();
        let _ = fs::remove_dir_all(&staging);
        validate_uv_runtime_candidate(UvRuntimeCandidate {
            source: UvRuntimeSource::Managed,
            uv: uv_binary_at(&target),
        })
        .await
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

fn uv_runtime_candidates(
    options: &AgentUvRuntimeOptions,
    system_uv: Option<PathBuf>,
) -> Vec<UvRuntimeCandidate> {
    let mut candidates = Vec::new();
    if let Some(uv) = options.uv_path.clone() {
        candidates.push(UvRuntimeCandidate {
            source: UvRuntimeSource::Explicit,
            uv,
        });
    }
    if let Some(uv) = system_uv {
        candidates.push(UvRuntimeCandidate {
            source: UvRuntimeSource::System,
            uv,
        });
    }
    candidates
}

fn write_isolated_npm_configs(staging: &Path) -> VibexResult<IsolatedNpmConfigs> {
    // npm rejects loading one file as both its user and global configuration.
    let configs = IsolatedNpmConfigs {
        user: staging.join("npm-user-config"),
        global: staging.join("npm-global-config"),
    };
    write_private_file(&configs.user, b"")?;
    write_private_file(&configs.global, b"")?;
    Ok(configs)
}

async fn select_valid_external_node_runtime(
    candidates: Vec<NodeRuntimeCandidate>,
    minimum_version: &semver::Version,
) -> Option<NodeRuntime> {
    for candidate in candidates {
        match validate_node_runtime_candidate(candidate.clone(), minimum_version).await {
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

async fn select_valid_external_uv_runtime(
    candidates: Vec<UvRuntimeCandidate>,
) -> Option<UvRuntime> {
    for candidate in candidates {
        match validate_uv_runtime_candidate(candidate.clone()).await {
            Ok(runtime) => return Some(runtime),
            Err(error) => tracing::warn!(
                target: "vibex_desktop",
                uv_source = candidate.source.as_str(),
                error_code = %error.code,
                "ACP Agent uv candidate was rejected"
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedCliAgent {
    Codewhale,
    Hermes,
    Kiro,
}

impl ManagedCliAgent {
    fn for_agent(agent_id: &AgentId) -> Option<Self> {
        match agent_id.as_str() {
            "codewhale" => Some(Self::Codewhale),
            "hermes" => Some(Self::Hermes),
            "kiro" => Some(Self::Kiro),
            _ => None,
        }
    }

    fn registry_id(self) -> &'static str {
        match self {
            Self::Codewhale => "codewhale",
            Self::Hermes => "hermes",
            Self::Kiro => "kiro",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RegistryDistribution {
    binary: Option<BTreeMap<String, RegistryBinaryTarget>>,
    npx: Option<RegistryNpxDistribution>,
    uvx: Option<RegistryUvxDistribution>,
    #[serde(skip)]
    kiro: Option<RegistryKiroDistribution>,
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

#[derive(Debug, Clone, Deserialize)]
struct RegistryUvxDistribution {
    package: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Clone)]
struct RegistryKiroDistribution {
    archive: String,
    sha256: String,
    cli_path: Option<String>,
    file_type: String,
    version: String,
}

#[derive(Debug, Clone)]
enum ResolvedDistribution {
    Binary(RegistryBinaryTarget),
    Npm(RegistryNpxDistribution),
    Uvx(RegistryUvxDistribution),
    Kiro(RegistryKiroDistribution),
}

impl ResolvedDistribution {
    fn kind(&self) -> AgentManagedDistributionKind {
        match self {
            Self::Binary(_) => AgentManagedDistributionKind::Binary,
            Self::Npm(_) => AgentManagedDistributionKind::Npm,
            Self::Uvx(_) => AgentManagedDistributionKind::Uvx,
            Self::Kiro(_) => AgentManagedDistributionKind::Binary,
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
struct PypiPackageMetadata {
    info: PypiPackageInfo,
}

#[derive(Debug, Deserialize)]
struct PypiPackageInfo {
    name: String,
    version: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KiroReleaseManifest {
    version: String,
    packages: Vec<KiroReleasePackage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KiroReleasePackage {
    os: String,
    architecture: String,
    variant: String,
    file_type: String,
    download: String,
    sha256: String,
    #[serde(default)]
    cli_path: Option<String>,
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

#[derive(Debug)]
struct VerifiedNpmPackage {
    name: String,
    version: String,
    integrity: String,
    tarball: String,
    bin_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedUvxPackage {
    name: String,
    extras: Vec<String>,
    version: String,
}

impl VerifiedUvxPackage {
    fn exact_spec(&self) -> String {
        let extras = (!self.extras.is_empty()).then(|| format!("[{}]", self.extras.join(",")));
        format!(
            "{}{}=={}",
            self.name,
            extras.as_deref().unwrap_or_default(),
            self.version
        )
    }
}

#[derive(Debug, Deserialize)]
struct InstalledUvxPackage {
    version: String,
    scripts: Vec<String>,
}

impl VerifiedNpmPackage {
    fn exact_spec(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallManifest {
    registry_agent_id: String,
    version: String,
    fingerprint: String,
    #[serde(default)]
    runtime_version: Option<String>,
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
    Python {
        python: String,
        script: String,
        args: Vec<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryCacheMetadata {
    fetched_at_ms: i64,
}

#[derive(Debug, PartialEq, Eq)]
struct IsolatedNpmConfigs {
    user: PathBuf,
    global: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeRuntimeSource {
    Explicit,
    System,
    Managed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UvRuntimeSource {
    Explicit,
    System,
    Managed,
}

impl UvRuntimeSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::System => "system",
            Self::Managed => "managed",
        }
    }
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

#[derive(Debug, Clone)]
struct UvRuntime {
    uv: PathBuf,
    version: semver::Version,
    source: UvRuntimeSource,
}

impl UvRuntime {
    fn command(&self) -> Command {
        Command::new(&self.uv)
    }

    fn fingerprint_identity(&self) -> String {
        format!(
            "{}\0{}\0{}",
            self.source.as_str(),
            self.version,
            self.uv.to_string_lossy()
        )
    }
}

#[derive(Debug, Clone)]
struct UvRuntimeCandidate {
    source: UvRuntimeSource,
    uv: PathBuf,
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

fn minimum_node_version(agent_id: &AgentId) -> semver::Version {
    if agent_id.as_str() == "pi" {
        PI_MINIMUM_NODE_VERSION
    } else {
        MINIMUM_NODE_VERSION
    }
}

fn latest_npm_companion(agent_id: &AgentId) -> Option<&'static str> {
    match agent_id.as_str() {
        "amp-acp" => Some(AMP_CLI_PACKAGE),
        "autohand" => Some(AUTOHAND_CLI_PACKAGE),
        "pi" => Some(PI_CODING_AGENT_PACKAGE),
        _ => None,
    }
}

fn managed_uvx_entry_point(agent_id: &AgentId) -> Option<&'static str> {
    match agent_id.as_str() {
        "hermes" => Some("hermes"),
        _ => None,
    }
}

fn npm_companion_command(agent_id: &AgentId) -> Option<&'static str> {
    match agent_id.as_str() {
        "amp-acp" => Some("amp"),
        "autohand" => Some("autohand"),
        "pi" => Some(PI_COMMAND_NAME),
        _ => None,
    }
}

fn npm_companion_environment(agent_id: &AgentId) -> Option<&'static str> {
    match agent_id.as_str() {
        "amp-acp" => Some("AMP_CLI_PATH"),
        "autohand" => Some("AUTOHAND_CMD"),
        "pi" => Some("PI_ACP_PI_COMMAND"),
        _ => None,
    }
}

fn npm_companion_command_for_registry_id(registry_agent_id: &str) -> Option<&'static str> {
    match registry_agent_id {
        "amp-acp" => Some("amp"),
        "autohand" => Some("autohand"),
        "pi-acp" => Some(PI_COMMAND_NAME),
        _ => None,
    }
}

fn resolve_distribution(entry: &RegistryEntry) -> VibexResult<ResolvedDistribution> {
    if let Some(kiro) = entry.distribution.kiro.clone() {
        return Ok(ResolvedDistribution::Kiro(kiro));
    }
    if let Some(targets) = entry.distribution.binary.as_ref()
        && let Some(target) = targets.get(current_platform_key()?)
    {
        return Ok(ResolvedDistribution::Binary(target.clone()));
    }
    if let Some(npx) = entry.distribution.npx.clone() {
        return Ok(ResolvedDistribution::Npm(npx));
    }
    if let Some(uvx) = entry.distribution.uvx.clone() {
        return Ok(ResolvedDistribution::Uvx(uvx));
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

fn uv_archive_filename() -> VibexResult<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("uv-x86_64-unknown-linux-gnu.tar.gz"),
        ("linux", "aarch64") => Ok("uv-aarch64-unknown-linux-gnu.tar.gz"),
        ("macos", "x86_64") => Ok("uv-x86_64-apple-darwin.tar.gz"),
        ("macos", "aarch64") => Ok("uv-aarch64-apple-darwin.tar.gz"),
        ("windows", "x86_64") => Ok("uv-x86_64-pc-windows-msvc.zip"),
        ("windows", "aarch64") => Ok("uv-aarch64-pc-windows-msvc.zip"),
        _ => Err(VibexError::capability(
            "agent_uv_platform_unsupported",
            "managed uv is unavailable on this platform",
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

fn uv_binary_at(root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        root.join("uv.exe")
    }
    #[cfg(not(windows))]
    {
        root.join("uv")
    }
}

fn uv_venv_python(venv: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        venv.join("Scripts/python.exe")
    }
    #[cfg(not(windows))]
    {
        venv.join("bin/python")
    }
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
    minimum_version: &semver::Version,
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
    validate_minimum_node_version(&version, minimum_version)?;

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

async fn validate_uv_runtime_candidate(candidate: UvRuntimeCandidate) -> VibexResult<UvRuntime> {
    if !candidate.uv.is_file() {
        return Err(VibexError::validation(
            "agent_uv_binary_missing",
            "uv candidate does not point to a file",
        ));
    }
    let mut version_command = Command::new(&candidate.uv);
    version_command.arg("--version");
    let output = run_process_probe(
        version_command,
        UV_PROBE_TIMEOUT,
        "agent_uv_version_probe_timeout",
        "agent_uv_version_probe_failed",
        "uv version could not be detected",
    )
    .await?;
    let version = parse_uv_version_output(&output.stdout)?;
    if version < MINIMUM_UV_VERSION {
        return Err(VibexError::capability(
            "agent_uv_version_unsupported",
            "uv candidate is older than the supported minimum",
        )
        .with_diagnostic("detectedVersion", version.to_string())
        .with_diagnostic("minimumVersion", MINIMUM_UV_VERSION.to_string()));
    }
    Ok(UvRuntime {
        uv: candidate.uv,
        version,
        source: candidate.source,
    })
}

fn validate_minimum_node_version(
    version: &semver::Version,
    minimum_version: &semver::Version,
) -> VibexResult<()> {
    if version < minimum_version {
        return Err(VibexError::capability(
            "agent_node_version_unsupported",
            "Node.js candidate is older than the supported minimum",
        )
        .with_diagnostic("detectedVersion", version.to_string())
        .with_diagnostic("minimumVersion", minimum_version.to_string()));
    }
    Ok(())
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
    command: Command,
    timeout_code: &str,
    failure_code: &str,
    message: &str,
) -> VibexResult<Output> {
    run_process_probe(
        command,
        NODE_PROBE_TIMEOUT,
        timeout_code,
        failure_code,
        message,
    )
    .await
}

async fn run_process_probe(
    mut command: Command,
    probe_timeout: Duration,
    timeout_code: &str,
    failure_code: &str,
    message: &str,
) -> VibexResult<Output> {
    let output = timeout(probe_timeout, command.output())
        .await
        .map_err(|_| VibexError::process(timeout_code, message))?
        .map_err(|error| process_error(failure_code, message, error))?;
    if !output.status.success() {
        return Err(VibexError::process(failure_code, message)
            .with_diagnostic("status", output.status.to_string()));
    }
    Ok(output)
}

async fn run_install_command(
    mut command: Command,
    timeout_code: &str,
    spawn_code: &str,
    failure_code: &str,
    message: &str,
) -> VibexResult<()> {
    let status = timeout(INSTALL_TIMEOUT, command.status())
        .await
        .map_err(|_| VibexError::process(timeout_code, message))?
        .map_err(|error| process_error(spawn_code, message, error))?;
    if !status.success() {
        return Err(VibexError::process(failure_code, message)
            .with_diagnostic("status", status.to_string()));
    }
    Ok(())
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

fn parse_uv_version_output(output: &[u8]) -> VibexResult<semver::Version> {
    let output = std::str::from_utf8(output).map_err(|error| {
        VibexError::validation(
            "agent_uv_version_output_invalid",
            "uv version output was not UTF-8",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let version = output
        .split_whitespace()
        .nth(1)
        .filter(|_| output.trim_start().starts_with("uv "))
        .ok_or_else(|| {
            VibexError::validation(
                "agent_uv_version_output_invalid",
                "uv version output was invalid",
            )
        })?;
    semver::Version::parse(version.trim_start_matches('v')).map_err(|error| {
        VibexError::validation(
            "agent_uv_version_output_invalid",
            "uv version output was invalid",
        )
        .with_diagnostic("error", error.to_string())
    })
}

async fn inspect_uvx_package(python: &Path, package: &str) -> VibexResult<InstalledUvxPackage> {
    const SCRIPT: &str = r#"
import importlib.metadata as metadata
import json
import sys

distribution = metadata.distribution(sys.argv[1])
scripts = sorted({entry.name for entry in distribution.entry_points if entry.group == "console_scripts"})
print(json.dumps({"version": distribution.version, "scripts": scripts}))
"#;

    let mut command = Command::new(python);
    // Ignore inherited Python configuration so metadata always comes from this venv.
    command.arg("-I").arg("-c").arg(SCRIPT).arg(package);
    let output = run_process_probe(
        command,
        UV_PROBE_TIMEOUT,
        "agent_uvx_metadata_probe_timeout",
        "agent_uvx_metadata_probe_failed",
        "installed uvx package metadata could not be read",
    )
    .await?;
    serde_json::from_slice(&output.stdout).map_err(|error| {
        VibexError::validation(
            "agent_uvx_metadata_invalid",
            "installed uvx package metadata was invalid",
        )
        .with_diagnostic("error", error.to_string())
    })
}

fn select_uvx_entry_point(package: &str, scripts: &[String]) -> VibexResult<String> {
    let expected = normalize_python_package_name(package);
    let matching = scripts
        .iter()
        .filter(|script| normalize_python_package_name(script) == expected)
        .collect::<Vec<_>>();
    let selected = match matching.as_slice() {
        [script] => (*script).clone(),
        [] if scripts.len() == 1 => scripts[0].clone(),
        _ => {
            return Err(VibexError::validation(
                "agent_uvx_entry_point_ambiguous",
                "uvx package did not expose an unambiguous executable",
            )
            .with_diagnostic("package", package.to_string()));
        }
    };
    if selected.is_empty()
        || !selected
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(VibexError::validation(
            "agent_uvx_entry_point_invalid",
            "uvx package executable name was invalid",
        ));
    }
    Ok(selected)
}

fn normalize_python_package_name(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_separator = false;
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() {
            normalized.push(byte.to_ascii_lowercase() as char);
            previous_separator = false;
        } else if !previous_separator {
            normalized.push('-');
            previous_separator = true;
        }
    }
    normalized.trim_matches('-').to_string()
}

fn uvx_launcher_source(package: &str, executable: &str) -> VibexResult<String> {
    let package = serde_json::to_string(package).map_err(|error| {
        VibexError::validation(
            "agent_uvx_launcher_encode_failed",
            "uvx package identity could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let executable = serde_json::to_string(executable).map_err(|error| {
        VibexError::validation(
            "agent_uvx_launcher_encode_failed",
            "uvx executable identity could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    Ok(format!(
        r#"from importlib.metadata import distribution
import sys

_distribution = distribution({package})
_entry_point = next(
    (
        entry
        for entry in _distribution.entry_points
        if entry.group == "console_scripts" and entry.name == {executable}
    ),
    None,
)
if _entry_point is None:
    raise SystemExit("Managed uvx executable is unavailable")
sys.exit(_entry_point.load()())
"#
    ))
}

fn ensure_uv_cache_directories(cache_dir: &Path, python_dir: &Path) -> VibexResult<()> {
    for (path, code, message) in [
        (
            cache_dir,
            "agent_uv_cache_create_failed",
            "managed uv cache could not be created",
        ),
        (
            python_dir,
            "agent_uv_python_cache_create_failed",
            "managed uv Python cache could not be created",
        ),
    ] {
        fs::create_dir_all(path).map_err(|error| storage_error(code, message, error))?;
    }
    Ok(())
}

fn configure_isolated_uv_command(command: &mut Command, cache_dir: &Path, python_dir: &Path) {
    for key in [
        "UV_CONFIG_FILE",
        "UV_INDEX",
        "UV_INDEX_URL",
        "UV_EXTRA_INDEX",
        "UV_EXTRA_INDEX_URL",
        "UV_FIND_LINKS",
        "UV_INSECURE_HOST",
        "UV_KEYRING_PROVIDER",
        "UV_PYTHON",
        "UV_PYTHON_DOWNLOADS",
        "UV_TOOL_DIR",
        "UV_TOOL_BIN_DIR",
        "UV_PROJECT_ENVIRONMENT",
        "VIRTUAL_ENV",
        "PYTHONHOME",
        "PYTHONPATH",
        "PIP_CONFIG_FILE",
        "PIP_INDEX_URL",
        "PIP_EXTRA_INDEX_URL",
        "PIP_FIND_LINKS",
        "PIP_TRUSTED_HOST",
    ] {
        command.env_remove(key);
    }
    command
        .env("UV_CACHE_DIR", cache_dir)
        .env("UV_PYTHON_INSTALL_DIR", python_dir)
        .env("UV_NO_CONFIG", "1")
        .env("UV_NO_ENV_FILE", "1")
        .env("UV_DEFAULT_INDEX", "https://pypi.org/simple")
        .env("UV_KEYRING_PROVIDER", "disabled")
        .env("UV_LINK_MODE", "copy")
        .env("UV_PYTHON_DOWNLOADS", "automatic");
}

fn ensure_managed_uv_python(
    managed_root: &Path,
    install_root: &Path,
    python: &Path,
) -> VibexResult<()> {
    if !python.starts_with(install_root) || !python.is_file() {
        return Err(VibexError::validation(
            "agent_uvx_python_missing",
            "managed uvx Python runtime was missing",
        ));
    }
    let python_root = managed_root.join("runtimes/python");
    let canonical_python_root = fs::canonicalize(&python_root).map_err(|error| {
        storage_error(
            "agent_uv_python_cache_canonicalize_failed",
            "managed uv Python cache could not be verified",
            error,
        )
    })?;
    let canonical_python = fs::canonicalize(python).map_err(|error| {
        storage_error(
            "agent_uvx_python_canonicalize_failed",
            "managed uvx Python runtime could not be verified",
            error,
        )
    })?;
    if !canonical_python.starts_with(canonical_python_root) {
        return Err(VibexError::validation(
            "agent_uvx_python_escape",
            "managed uvx Python runtime escaped the managed cache",
        ));
    }
    Ok(())
}

fn ensure_managed_uv_python_for_install(install_root: &Path, python: &Path) -> VibexResult<()> {
    let service_root = install_root
        .ancestors()
        .nth(3)
        .filter(|_| {
            install_root
                .ancestors()
                .nth(2)
                .and_then(Path::file_name)
                .is_some_and(|name| name == "agents")
        })
        .ok_or_else(|| {
            VibexError::validation(
                "agent_uvx_install_root_invalid",
                "managed uvx installation root was invalid",
            )
        })?;
    ensure_managed_uv_python(service_root, install_root, python)
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

async fn run_trusted_npm_setup(
    agent_id: &AgentId,
    staging: &Path,
    node: &NodeRuntime,
    npm_config: &IsolatedNpmConfigs,
) -> VibexResult<()> {
    let setup = match agent_id.as_str() {
        "amp-acp" => Some((AMP_CLI_PACKAGE, "install.cjs")),
        "autohand" => Some((
            AUTOHAND_CLI_PACKAGE,
            "scripts/ensure-node-pty-helper-permissions.mjs",
        )),
        "codewhale" => Some((CODEWHALE_CLI_PACKAGE, "scripts/install.js")),
        _ => None,
    };
    let Some((package, script)) = setup else {
        return Ok(());
    };
    let package_root = package_directory(staging, package)?;
    let script = package_root.join(safe_relative_path(script, "npm setup script")?);
    ensure_regular_file(staging, &script, "agent_npm_setup_script_missing")?;
    let mut command = Command::new(&node.node);
    command
        .arg(&script)
        .current_dir(package_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .env("npm_config_cache", staging.join("npm-setup-cache"))
        .env("npm_config_userconfig", &npm_config.user)
        .env("npm_config_globalconfig", &npm_config.global)
        .env("npm_config_update_notifier", "false")
        .env_remove("NPM_TOKEN")
        .env_remove("NODE_AUTH_TOKEN");
    prepend_node_to_path(&mut command, &node.node);
    run_install_command(
        command,
        "agent_npm_setup_timeout",
        "agent_npm_setup_spawn_failed",
        "agent_npm_setup_failed",
        "managed npm Agent setup failed",
    )
    .await
}

fn validate_npm_version(version: &str, label: &str) -> VibexResult<()> {
    validate_safe_segment(version, label)?;
    semver::Version::parse(version).map_err(|error| {
        VibexError::validation(
            "agent_managed_version_invalid",
            format!("{label} was not exact SemVer"),
        )
        .with_diagnostic("error", error.to_string())
    })?;
    Ok(())
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

fn parse_exact_uvx_spec(spec: &str, expected: &str) -> VibexResult<VerifiedUvxPackage> {
    let (package, version) = if let Some((package, version)) = spec.rsplit_once("==") {
        (package, version)
    } else if let Some((package, version)) = spec.rsplit_once('@') {
        (package, version)
    } else {
        return Err(VibexError::validation(
            "agent_uvx_spec_not_exact",
            "ACP Registry uvx package must include an exact version",
        ));
    };
    let (package, extras) = if let Some((package, extras)) = package.split_once('[') {
        let extras = extras.strip_suffix(']').ok_or_else(|| {
            VibexError::validation(
                "agent_uvx_spec_invalid",
                "ACP Registry uvx package extras were invalid",
            )
        })?;
        let extras = extras.split(',').map(str::to_string).collect::<Vec<_>>();
        if extras.is_empty()
            || extras.iter().any(|extra| {
                extra.is_empty()
                    || !extra.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            })
        {
            return Err(VibexError::validation(
                "agent_uvx_spec_invalid",
                "ACP Registry uvx package extras were invalid",
            ));
        }
        (package, extras)
    } else {
        (package, Vec::new())
    };
    if package.is_empty()
        || version.is_empty()
        || version != expected
        || package.contains("..")
        || package.contains(['[', ']'])
        || !package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(VibexError::validation(
            "agent_uvx_spec_invalid",
            "ACP Registry uvx package identity was invalid",
        ));
    }
    semver::Version::parse(version).map_err(|error| {
        VibexError::validation(
            "agent_uvx_version_invalid",
            "ACP Registry uvx package version was not exact semver",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    Ok(VerifiedUvxPackage {
        name: package.to_string(),
        extras,
        version: version.to_string(),
    })
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
            bins.get(if package == AUTOHAND_CLI_PACKAGE {
                "autohand"
            } else {
                preferred
            })
            .or_else(|| {
                let mut paths = bins.values();
                let path = paths.next()?;
                paths.all(|candidate| candidate == path).then_some(path)
            })
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

fn npm_package_bin_relative_path(package: &VerifiedNpmPackage) -> VibexResult<PathBuf> {
    Ok(package_directory(Path::new(""), &package.name)?
        .join(safe_relative_path(&package.bin_path, "npm bin")?))
}

fn npm_command_path(root: &Path, command: &str) -> PathBuf {
    #[cfg(windows)]
    let command = format!("{command}.cmd");
    root.join("node_modules").join(".bin").join(command)
}

fn select_kiro_package(packages: &[KiroReleasePackage]) -> VibexResult<KiroReleasePackage> {
    let os = env::consts::OS;
    let architecture = env::consts::ARCH;
    let selected = packages
        .iter()
        .filter(|package| package.os == os)
        .find(|package| match os {
            "linux" => {
                package.architecture == architecture
                    && package.variant == "headless"
                    && package.file_type == "tarGz"
                    && package.download.ends_with(".tar.gz")
                    && (cfg!(target_env = "musl") == package.download.contains("-musl"))
            }
            "macos" => {
                package.architecture == "universal"
                    && package.variant == "full"
                    && package.file_type == "dmg"
            }
            "windows" => {
                package.architecture == architecture
                    && package.variant == "full"
                    && package.file_type == "msi"
            }
            _ => false,
        })
        .cloned()
        .ok_or_else(|| {
            VibexError::capability(
                "agent_kiro_platform_unsupported",
                "Kiro CLI did not publish a supported package for this platform",
            )
            .with_diagnostic("os", os.to_string())
            .with_diagnostic("architecture", architecture.to_string())
        })?;
    validate_safe_segment(&selected.file_type, "Kiro CLI archive format")?;
    validate_sha256(&selected.sha256)?;
    Ok(selected)
}

fn kiro_archive_url(download: &str, version: &str) -> VibexResult<String> {
    let download_path = safe_relative_path(download, "Kiro CLI download")?;
    if download_path.parent() != Some(Path::new(version)) {
        return Err(VibexError::validation(
            "agent_kiro_download_version_mismatch",
            "Kiro CLI download path did not match the latest manifest version",
        ));
    }
    let filename = download_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            VibexError::validation(
                "agent_kiro_download_invalid",
                "Kiro CLI download did not have a valid filename",
            )
        })?;
    Url::parse(&format!("{KIRO_DOWNLOAD_BASE_URL}/"))
        .and_then(|base| base.join(filename))
        .map(|url| url.to_string())
        .map_err(|error| {
            VibexError::validation(
                "agent_kiro_download_invalid",
                "Kiro CLI download URL could not be constructed",
            )
            .with_diagnostic("error", error.to_string())
        })
}

fn npm_companion_launcher_source(agent_id: &AgentId, adapter_script: &Path) -> VibexResult<String> {
    let command = npm_companion_command(agent_id).ok_or_else(|| {
        VibexError::validation(
            "agent_npm_launcher_agent_invalid",
            "managed npm companion launcher did not have a command",
        )
    })?;
    let environment = npm_companion_environment(agent_id).ok_or_else(|| {
        VibexError::validation(
            "agent_npm_launcher_agent_invalid",
            "managed npm companion launcher did not have an environment key",
        )
    })?;
    let adapter_script = adapter_script.to_str().ok_or_else(|| {
        VibexError::validation(
            "agent_npm_launcher_path_invalid",
            "ACP adapter path was not valid UTF-8",
        )
    })?;
    let adapter_script = serde_json::to_string(adapter_script).map_err(|error| {
        VibexError::validation(
            "agent_npm_launcher_path_invalid",
            "ACP adapter path could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let windows_command = serde_json::to_string(&format!("{command}.cmd")).map_err(|error| {
        VibexError::validation(
            "agent_npm_launcher_command_invalid",
            "managed npm companion command could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let command = serde_json::to_string(command).map_err(|error| {
        VibexError::validation(
            "agent_npm_launcher_command_invalid",
            "managed npm companion command could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let environment = serde_json::to_string(environment).map_err(|error| {
        VibexError::validation(
            "agent_npm_launcher_environment_invalid",
            "managed npm companion environment key could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let label = serde_json::to_string(agent_id.as_str()).map_err(|error| {
        VibexError::validation(
            "agent_npm_launcher_agent_invalid",
            "managed npm Agent id could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    Ok(format!(
        r#""use strict";
const path = require("node:path");
const {{ pathToFileURL }} = require("node:url");

const companionCommand = process.platform === "win32" ? {windows_command} : {command};
process.env[{environment}] = path.join(__dirname, "node_modules", ".bin", companionCommand);
const adapter = path.join(__dirname, {adapter_script});
import(pathToFileURL(adapter).href).catch((error) => {{
  console.error("Failed to start {label} ACP adapter:", error);
  process.exitCode = 1;
}});
"#
    ))
}

fn npm_companion_binary_launcher_source(
    agent_id: &AgentId,
    adapter_binary: &Path,
) -> VibexResult<String> {
    let command = npm_companion_command(agent_id).ok_or_else(|| {
        VibexError::validation(
            "agent_npm_launcher_agent_invalid",
            "managed npm companion launcher did not have a command",
        )
    })?;
    let environment = npm_companion_environment(agent_id).ok_or_else(|| {
        VibexError::validation(
            "agent_npm_launcher_agent_invalid",
            "managed npm companion launcher did not have an environment key",
        )
    })?;
    let adapter_binary = adapter_binary.to_str().ok_or_else(|| {
        VibexError::validation(
            "agent_npm_launcher_path_invalid",
            "ACP adapter path was not valid UTF-8",
        )
    })?;
    let adapter_binary = serde_json::to_string(adapter_binary).map_err(|error| {
        VibexError::validation(
            "agent_npm_launcher_path_invalid",
            "ACP adapter path could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let windows_command = serde_json::to_string(&format!("{command}.cmd")).map_err(|error| {
        VibexError::validation(
            "agent_npm_launcher_command_invalid",
            "managed npm companion command could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let command = serde_json::to_string(command).map_err(|error| {
        VibexError::validation(
            "agent_npm_launcher_command_invalid",
            "managed npm companion command could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let environment = serde_json::to_string(environment).map_err(|error| {
        VibexError::validation(
            "agent_npm_launcher_environment_invalid",
            "managed npm companion environment key could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let label = serde_json::to_string(agent_id.as_str()).map_err(|error| {
        VibexError::validation(
            "agent_npm_launcher_agent_invalid",
            "managed npm Agent id could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    Ok(format!(
        r#""use strict";
const path = require("node:path");
const {{ spawn }} = require("node:child_process");

const companionCommand = process.platform === "win32" ? {windows_command} : {command};
process.env[{environment}] = path.join(__dirname, "node_modules", ".bin", companionCommand);
const adapter = path.join(__dirname, {adapter_binary});
const child = spawn(adapter, process.argv.slice(2), {{ stdio: "inherit", env: process.env }});
child.on("error", (error) => {{
  console.error("Failed to start {label} ACP adapter:", error);
  process.exitCode = 1;
}});
child.on("close", (code, signal) => {{
  if (signal) {{
    process.kill(process.pid, signal);
  }} else {{
    process.exitCode = code ?? 1;
  }}
}});
"#
    ))
}

#[cfg(test)]
fn pi_acp_launcher_source(adapter_script: &Path) -> VibexResult<String> {
    npm_companion_launcher_source(
        &AgentId::parse("pi").expect("static Agent id"),
        adapter_script,
    )
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
    if matches!(
        manifest.registry_agent_id.as_str(),
        "amp-acp" | "autohand" | "pi-acp"
    ) && manifest.runtime_version.is_none()
    {
        return Err(VibexError::validation(
            "agent_npm_runtime_version_missing",
            "managed npm companion version was missing from the installation",
        ));
    }
    if let Some(companion) = npm_companion_command_for_registry_id(&manifest.registry_agent_id) {
        ensure_regular_file(
            root,
            &npm_command_path(root, companion),
            "agent_npm_runtime_command_missing",
        )?;
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
        ManifestLaunch::Python {
            python,
            script,
            args,
        } => {
            let python = root.join(safe_relative_path(&python, "uvx Python runtime")?);
            ensure_managed_uv_python_for_install(root, &python)?;
            let script = root.join(safe_relative_path(&script, "uvx launcher")?);
            ensure_regular_file(root, &script, "agent_uvx_launcher_missing")?;
            let mut launch_args = vec![script.to_string_lossy().into_owned()];
            launch_args.extend(args);
            AgentCommandConfig {
                command: python.to_string_lossy().into_owned(),
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

fn managed_companion_installation_is_usable(
    agent_id: &AgentId,
    record: &AgentManagedInstallationRecord,
) -> bool {
    if latest_npm_companion(agent_id).is_none() {
        return true;
    }
    let Some(root) = record.install_root.as_deref().map(Path::new) else {
        return false;
    };
    let runtime_version_is_valid = read_manifest_runtime_version(root)
        .is_some_and(|version| semver::Version::parse(&version).is_ok());
    let runtime_command_is_valid = npm_companion_command(agent_id).is_some_and(|command| {
        ensure_regular_file(
            root,
            &npm_command_path(root, command),
            "agent_npm_runtime_command_missing",
        )
        .is_ok()
    });
    runtime_version_is_valid && runtime_command_is_valid
}

fn installation_files_are_usable(record: &AgentManagedInstallationRecord) -> bool {
    record
        .install_root
        .as_deref()
        .is_some_and(|root| Path::new(root).is_dir())
        && record.command.as_ref().is_some_and(command_is_available)
}

fn installed_distribution_kind(root: &Path) -> Option<AgentManagedDistributionKind> {
    fs::read(root.join("vibex-install.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<InstallManifest>(&bytes).ok())
        .map(|manifest| manifest.distribution_kind)
}

fn record_matches_distribution(
    record: &AgentManagedInstallationRecord,
    expected: AgentManagedDistributionKind,
) -> bool {
    record.state.distribution_kind == Some(expected)
        && record
            .install_root
            .as_deref()
            .is_some_and(|root| installed_distribution_kind(Path::new(root)) == Some(expected))
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

fn read_manifest_runtime_version(root: &Path) -> Option<String> {
    let bytes = fs::read(root.join("vibex-install.json")).ok()?;
    let manifest = serde_json::from_slice::<InstallManifest>(&bytes).ok()?;
    manifest.runtime_version
}

fn command_is_available(command: &AgentCommandConfig) -> bool {
    let program = Path::new(&command.command);
    let script_runtime = program.file_stem().is_some_and(|name| {
        matches!(
            name.to_string_lossy().as_ref(),
            "node" | "python" | "python3"
        )
    });
    program.is_file()
        && (!script_runtime
            || command
                .args
                .first()
                .is_some_and(|script| Path::new(script).is_file()))
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

fn optional_sha256(value: Option<&str>) -> VibexResult<Option<String>> {
    value
        .filter(|value| !value.is_empty())
        .map(validate_sha256)
        .transpose()
}

fn parse_uv_release_checksum(bytes: &[u8], filename: &str) -> VibexResult<String> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        VibexError::validation(
            "agent_uv_checksum_invalid",
            "uv checksum response was not UTF-8",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let checksum = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let checksum = fields.next()?;
            let artifact = fields.next()?.trim_start_matches('*');
            (artifact == filename && fields.next().is_none()).then_some(checksum)
        })
        .next()
        .ok_or_else(|| {
            VibexError::validation(
                "agent_uv_checksum_missing",
                "uv checksum response did not include the requested archive",
            )
        })?;
    validate_sha256(checksum)
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
        let kind = entry.header().entry_type();
        let relative = match safe_relative_path(&relative.to_string_lossy(), "archive entry") {
            Ok(relative) => relative,
            // Release tarballs commonly begin with a `./` directory record. It has no
            // destination-relative name, but is safe to ignore as a directory only.
            Err(_error)
                if kind.is_dir()
                    && relative
                        .components()
                        .all(|component| matches!(component, Component::CurDir)) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let output = destination.join(relative);
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

#[cfg(target_os = "macos")]
async fn install_kiro_macos(
    archive: &Path,
    staging: &Path,
    cli_path: Option<&str>,
) -> VibexResult<PathBuf> {
    let relative_cli = safe_relative_path(
        cli_path.ok_or_else(|| {
            VibexError::validation(
                "agent_kiro_cli_path_missing",
                "Kiro CLI macOS manifest did not provide a CLI path",
            )
        })?,
        "Kiro CLI path",
    )?;
    let mount = staging.join("kiro-mount");
    fs::create_dir_all(&mount).map_err(|error| {
        storage_error(
            "agent_kiro_mount_create_failed",
            "Kiro CLI DMG mount directory could not be created",
            error,
        )
    })?;
    let mut attach = Command::new("hdiutil");
    attach
        .arg("attach")
        .arg("-nobrowse")
        .arg("-readonly")
        .arg("-mountpoint")
        .arg(&mount)
        .arg(archive)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    run_install_command(
        attach,
        "agent_kiro_mount_timeout",
        "agent_kiro_mount_spawn_failed",
        "agent_kiro_mount_failed",
        "Kiro CLI DMG could not be mounted",
    )
    .await?;

    let mut source_app = None;
    if let Ok(entries) = fs::read_dir(&mount) {
        for entry in entries.flatten() {
            let candidate = entry.path();
            if candidate
                .extension()
                .is_some_and(|extension| extension == "app")
            {
                let path = candidate.join(&relative_cli);
                if path.is_file() {
                    source_app = Some(candidate);
                    break;
                }
            }
        }
    }
    let destination_app = staging.join("Kiro CLI.app");
    let destination = destination_app.join(&relative_cli);
    let copy_result = if let Some(source_app) = source_app {
        let mut copy = Command::new("ditto");
        copy.arg(source_app)
            .arg(&destination_app)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        run_install_command(
            copy,
            "agent_kiro_bundle_copy_timeout",
            "agent_kiro_bundle_copy_spawn_failed",
            "agent_kiro_bundle_copy_failed",
            "Kiro CLI app bundle could not be copied from the DMG",
        )
        .await
        .map(|()| destination.clone())
    } else {
        Err(VibexError::validation(
            "agent_kiro_binary_missing",
            "Kiro CLI DMG did not contain the manifest CLI path",
        ))
    };
    let mut detach = Command::new("hdiutil");
    detach
        .arg("detach")
        .arg(&mount)
        .arg("-force")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let detach_result = run_install_command(
        detach,
        "agent_kiro_unmount_timeout",
        "agent_kiro_unmount_spawn_failed",
        "agent_kiro_unmount_failed",
        "Kiro CLI DMG could not be unmounted",
    )
    .await;
    copy_result.and(detach_result.map(|()| destination))
}

#[cfg(target_os = "windows")]
async fn install_kiro_windows(archive: &Path, staging: &Path) -> VibexResult<PathBuf> {
    let extraction = staging.join("kiro-msi");
    fs::create_dir_all(&extraction).map_err(|error| {
        storage_error(
            "agent_kiro_msi_directory_failed",
            "Kiro CLI MSI extraction directory could not be created",
            error,
        )
    })?;
    let target = format!("TARGETDIR={}", extraction.to_string_lossy());
    let mut command = Command::new("msiexec");
    command
        .arg("/a")
        .arg(archive)
        .arg("/qn")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    run_install_command(
        command,
        "agent_kiro_msi_timeout",
        "agent_kiro_msi_spawn_failed",
        "agent_kiro_msi_failed",
        "Kiro CLI MSI extraction failed",
    )
    .await?;
    find_file_named(&extraction, "kiro-cli.exe").ok_or_else(|| {
        VibexError::validation(
            "agent_kiro_binary_missing",
            "Kiro CLI MSI did not contain kiro-cli.exe",
        )
    })
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn find_file_named(root: &Path, name: &str) -> Option<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut inspected = 0_usize;
    while let Some(path) = pending.pop() {
        inspected = inspected.saturating_add(1);
        if inspected > ARCHIVE_MAX_ENTRIES {
            return None;
        }
        let metadata = fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            if path
                .file_name()
                .is_some_and(|value| value.to_string_lossy().eq_ignore_ascii_case(name))
            {
                return Some(path);
            }
            continue;
        }
        for entry in fs::read_dir(path).ok()?.flatten() {
            pending.push(entry.path());
        }
    }
    None
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
    registry_cache_fetched_at_ms(path)
        .and_then(|fetched_at_ms| now.checked_sub(fetched_at_ms))
        .is_some_and(|age| (0..=REGISTRY_CACHE_MAX_AGE_MS).contains(&age))
}

fn registry_cache_fetched_at_ms(path: &Path) -> Option<i64> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RegistryCacheMetadata>(&bytes).ok())
        .map(|metadata| metadata.fetched_at_ms)
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

    #[tokio::test]
    async fn agent_operation_locks_only_serialize_the_same_agent() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("vibex.db");
        let root = temp.path().join("managed-agents");
        let service =
            AgentInstallService::new(&db_path, &root, ProviderConfigService::new(&db_path))
                .unwrap();
        let codex = AgentId::parse("codex").unwrap();
        let claude = AgentId::parse("claude").unwrap();

        let codex_guard = service.acquire_agent_operation(&codex).await;
        let waiting_service = service.clone();
        let waiting_codex = codex.clone();
        let same_agent = tokio::spawn(async move {
            waiting_service
                .acquire_agent_operation(&waiting_codex)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!same_agent.is_finished());

        let claude_guard = tokio::time::timeout(
            Duration::from_millis(250),
            service.acquire_agent_operation(&claude),
        )
        .await
        .expect("a different Agent operation must not wait for Codex");
        assert!(!same_agent.is_finished());

        drop(claude_guard);
        drop(codex_guard);
        let same_agent_guard = tokio::time::timeout(Duration::from_millis(250), same_agent)
            .await
            .expect("the queued Codex operation should resume")
            .expect("the queued Codex operation task should complete");
        drop(same_agent_guard);
    }

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
    fn write_uv_version_probe(path: &Path, version: &str) {
        use std::os::unix::fs::PermissionsExt as _;

        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf '%s\\n' 'uv {version}'; exit 0; fi\nexit 1\n"
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
        let runtime = select_valid_external_node_runtime(candidates, &MINIMUM_NODE_VERSION)
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
        let runtime = select_valid_external_node_runtime(
            node_runtime_candidates(&options, Some(system_node.clone()), Some(system_npm)),
            &MINIMUM_NODE_VERSION,
        )
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
            select_valid_external_node_runtime(
                node_runtime_candidates(&options, Some(system_node), Some(system_npm)),
                &MINIMUM_NODE_VERSION,
            )
            .await
            .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn uv_runtime_selection_prefers_valid_explicit_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let explicit_uv = temp.path().join("explicit-uv");
        let system_uv = temp.path().join("system-uv");
        write_uv_version_probe(&explicit_uv, "0.11.4");
        write_uv_version_probe(&system_uv, "0.10.0");

        let runtime = select_valid_external_uv_runtime(uv_runtime_candidates(
            &AgentUvRuntimeOptions {
                uv_path: Some(explicit_uv.clone()),
            },
            Some(system_uv),
        ))
        .await
        .expect("explicit uv runtime should be selected");

        assert_eq!(runtime.source, UvRuntimeSource::Explicit);
        assert_eq!(runtime.uv, explicit_uv);
        assert_eq!(runtime.version, semver::Version::new(0, 11, 4));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_explicit_uv_falls_back_to_valid_system_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let explicit_uv = temp.path().join("explicit-uv");
        let system_uv = temp.path().join("system-uv");
        write_uv_version_probe(&explicit_uv, "0.4.0");
        write_uv_version_probe(&system_uv, "0.11.4");

        let runtime = select_valid_external_uv_runtime(uv_runtime_candidates(
            &AgentUvRuntimeOptions {
                uv_path: Some(explicit_uv),
            },
            Some(system_uv.clone()),
        ))
        .await
        .expect("system uv runtime should be selected");

        assert_eq!(runtime.source, UvRuntimeSource::System);
        assert_eq!(runtime.uv, system_uv);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pi_rejects_node_older_than_its_runtime_dependency() {
        let temp = tempfile::tempdir().unwrap();
        let old_node = temp.path().join("old-node");
        let old_npm = temp.path().join("old-npm");
        let supported_node = temp.path().join("supported-node");
        let supported_npm = temp.path().join("supported-npm");
        write_version_probe(&old_node, "v22.18.0");
        write_version_probe(&old_npm, "10.9.2");
        write_version_probe(&supported_node, "v22.19.0");
        write_version_probe(&supported_npm, "10.9.2");

        let runtime = select_valid_external_node_runtime(
            vec![
                NodeRuntimeCandidate {
                    source: NodeRuntimeSource::Explicit,
                    node: old_node,
                    npm: old_npm,
                },
                NodeRuntimeCandidate {
                    source: NodeRuntimeSource::System,
                    node: supported_node.clone(),
                    npm: supported_npm,
                },
            ],
            &minimum_node_version(&AgentId::parse("pi").unwrap()),
        )
        .await
        .expect("Pi-compatible runtime should be selected");

        assert_eq!(runtime.source, NodeRuntimeSource::System);
        assert_eq!(runtime.node, supported_node);
        assert_eq!(runtime.version, PI_MINIMUM_NODE_VERSION);
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
    fn uv_version_probe_parses_and_rejects_malformed_output() {
        assert_eq!(
            parse_uv_version_output(b"uv 0.11.4 (x86_64-unknown-linux-gnu)\n").unwrap(),
            semver::Version::new(0, 11, 4)
        );
        assert!(parse_uv_version_output(b"0.11.4\n").is_err());
    }

    #[test]
    fn isolated_npm_configs_use_distinct_empty_files() {
        let temp = tempfile::tempdir().unwrap();

        let configs = write_isolated_npm_configs(temp.path()).unwrap();

        assert_ne!(configs.user, configs.global);
        assert_eq!(fs::read(configs.user).unwrap(), b"");
        assert_eq!(fs::read(configs.global).unwrap(), b"");
    }

    #[test]
    fn registry_aliases_and_latest_managed_agents_are_explicit() {
        assert_eq!(
            require_registry_id(&AgentId::parse("claude").unwrap()).unwrap(),
            "claude-acp"
        );
        assert_eq!(
            require_registry_id(&AgentId::parse("copilot").unwrap()).unwrap(),
            "github-copilot-cli"
        );
        assert_eq!(
            require_registry_id(&AgentId::parse("cursor").unwrap()).unwrap(),
            "cursor"
        );
        assert_eq!(
            require_registry_id(&AgentId::parse("pi").unwrap()).unwrap(),
            "pi-acp"
        );
        assert_eq!(
            require_registry_id(&AgentId::parse("fast-agent").unwrap()).unwrap(),
            "fast-agent"
        );
        assert_eq!(
            require_registry_id(&AgentId::parse("minion-code").unwrap()).unwrap(),
            "minion-code"
        );
        for agent_id in ["codewhale", "hermes", "kiro"] {
            assert_eq!(
                require_registry_id(&AgentId::parse(agent_id).unwrap()).unwrap(),
                agent_id
            );
        }
    }

    #[test]
    fn pi_latest_runtime_and_launcher_are_managed_locally() {
        let agent_id = AgentId::parse("pi").unwrap();
        let package = latest_npm_companion(&agent_id).unwrap();
        assert_eq!(package, PI_CODING_AGENT_PACKAGE);
        assert_eq!(
            parse_exact_npm_spec(&format!("{package}@1.2.3"), "1.2.3").unwrap(),
            (package, "1.2.3")
        );
        assert!(latest_npm_companion(&AgentId::parse("gemini").unwrap()).is_none());

        let launcher =
            pi_acp_launcher_source(Path::new("node_modules/pi-acp/dist/index.js")).unwrap();
        assert!(launcher.contains("PI_ACP_PI_COMMAND"));
        assert!(launcher.contains("node_modules\", \".bin"));
        assert!(launcher.contains("node_modules/pi-acp/dist/index.js"));
    }

    #[test]
    fn latest_npm_companions_are_bound_to_private_adapter_launchers() {
        for (agent, package, command, environment) in [
            ("amp-acp", AMP_CLI_PACKAGE, "amp", "AMP_CLI_PATH"),
            ("autohand", AUTOHAND_CLI_PACKAGE, "autohand", "AUTOHAND_CMD"),
            ("pi", PI_CODING_AGENT_PACKAGE, "pi", "PI_ACP_PI_COMMAND"),
        ] {
            let agent_id = AgentId::parse(agent).unwrap();
            assert_eq!(latest_npm_companion(&agent_id), Some(package));
            assert_eq!(npm_companion_command(&agent_id), Some(command));
            assert_eq!(npm_companion_environment(&agent_id), Some(environment));
            let launcher = npm_companion_launcher_source(
                &agent_id,
                Path::new("node_modules/adapter/dist/index.js"),
            )
            .unwrap();
            assert!(launcher.contains(environment));
            assert!(launcher.contains(&format!("? \"{command}.cmd\" : \"{command}\"")));
            assert!(launcher.contains("node_modules/adapter/dist/index.js"));
        }
    }

    #[test]
    fn amp_binary_launcher_uses_the_private_cli_and_forwards_acp_arguments() {
        let launcher = npm_companion_binary_launcher_source(
            &AgentId::parse("amp-acp").unwrap(),
            Path::new("amp-acp"),
        )
        .unwrap();

        assert!(launcher.contains("AMP_CLI_PATH"));
        assert!(launcher.contains("node_modules\", \".bin"));
        assert!(launcher.contains("const adapter = path.join(__dirname, \"amp-acp\")"));
        assert!(launcher.contains("spawn(adapter, process.argv.slice(2)"));
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
    fn exact_uvx_specs_accept_registry_forms_and_reject_ranges() {
        assert_eq!(
            parse_exact_uvx_spec("fast-agent-acp==0.9.30", "0.9.30").unwrap(),
            VerifiedUvxPackage {
                name: "fast-agent-acp".to_string(),
                extras: Vec::new(),
                version: "0.9.30".to_string(),
            }
        );
        assert_eq!(
            parse_exact_uvx_spec("minion-code@0.1.44", "0.1.44")
                .unwrap()
                .exact_spec(),
            "minion-code==0.1.44"
        );
        assert_eq!(
            parse_exact_uvx_spec("hermes-agent[acp]==0.19.0", "0.19.0")
                .unwrap()
                .exact_spec(),
            "hermes-agent[acp]==0.19.0"
        );
        assert!(parse_exact_uvx_spec("hermes-agent[acp,../bad]==0.19.0", "0.19.0").is_err());
        assert!(parse_exact_uvx_spec("agent>=1.2.3", "1.2.3").is_err());
        assert!(parse_exact_uvx_spec("agent@1.2.4", "1.2.3").is_err());
        assert!(parse_exact_uvx_spec("agent@https://example.com/agent.whl", "1.2.3").is_err());
    }

    #[test]
    fn distribution_accepts_platform_binary_without_checksum() {
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
                uvx: None,
                kiro: None,
            },
        };
        assert!(matches!(
            resolve_distribution(&entry).unwrap(),
            ResolvedDistribution::Binary(_)
        ));
        assert_eq!(optional_sha256(None).unwrap(), None);
        assert_eq!(optional_sha256(Some("")).unwrap(), None);
        assert_eq!(
            optional_sha256(Some(&"A".repeat(64))).unwrap(),
            Some("a".repeat(64))
        );
        assert!(optional_sha256(Some("not-a-checksum")).is_err());

        let fallback = RegistryEntry {
            distribution: RegistryDistribution {
                binary: None,
                npx: Some(RegistryNpxDistribution {
                    package: "test-agent@1.2.3".to_string(),
                    args: Vec::new(),
                }),
                uvx: None,
                kiro: None,
            },
            ..entry
        };
        assert!(matches!(
            resolve_distribution(&fallback).unwrap(),
            ResolvedDistribution::Npm(_)
        ));

        let uvx = RegistryEntry {
            distribution: RegistryDistribution {
                binary: None,
                npx: None,
                uvx: Some(RegistryUvxDistribution {
                    package: "test-agent==1.2.3".to_string(),
                    args: vec!["acp".to_string()],
                }),
                kiro: None,
            },
            ..fallback
        };
        assert!(matches!(
            resolve_distribution(&uvx).unwrap(),
            ResolvedDistribution::Uvx(_)
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

    #[cfg(target_os = "linux")]
    #[test]
    fn kiro_package_selection_uses_the_current_linux_runtime() {
        let architecture = env::consts::ARCH.to_string();
        let package = |download: &str| KiroReleasePackage {
            os: "linux".to_string(),
            architecture: architecture.clone(),
            variant: "headless".to_string(),
            file_type: "tarGz".to_string(),
            download: download.to_string(),
            sha256: "a".repeat(64),
            cli_path: None,
        };
        let packages = vec![
            package(&format!("1.2.3/kirocli-{architecture}-linux.tar.gz")),
            package(&format!("1.2.3/kirocli-{architecture}-linux-musl.tar.gz")),
        ];

        let selected = select_kiro_package(&packages).unwrap();
        assert_eq!(
            selected.download.contains("-musl"),
            cfg!(target_env = "musl")
        );
        assert_eq!(
            kiro_archive_url("2.16.2/Kiro CLI.dmg", "2.16.2").unwrap(),
            "https://prod.download.cli.kiro.dev/stable/latest/Kiro%20CLI.dmg"
        );
        assert!(kiro_archive_url("2.16.1/kirocli.zip", "2.16.2").is_err());
    }

    #[test]
    fn uv_release_checksum_requires_the_requested_artifact() {
        let checksum = "a".repeat(64);
        assert_eq!(
            parse_uv_release_checksum(
                format!("{checksum}  uv-x86_64-unknown-linux-gnu.tar.gz\n").as_bytes(),
                "uv-x86_64-unknown-linux-gnu.tar.gz"
            )
            .unwrap(),
            checksum
        );
        assert!(
            parse_uv_release_checksum(
                b"not-a-checksum  uv-x86_64-unknown-linux-gnu.tar.gz\n",
                "uv-x86_64-unknown-linux-gnu.tar.gz"
            )
            .is_err()
        );
        assert!(parse_uv_release_checksum(
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  another.tar.gz\n",
            "uv-x86_64-unknown-linux-gnu.tar.gz"
        )
        .is_err());
    }

    #[test]
    fn uvx_entry_point_selection_is_deterministic_and_fail_closed() {
        assert_eq!(
            select_uvx_entry_point(
                "fast-agent-acp",
                &["fast_agent_acp".to_string(), "another".to_string()]
            )
            .unwrap(),
            "fast_agent_acp"
        );
        assert_eq!(
            select_uvx_entry_point("package", &["run-agent".to_string()]).unwrap(),
            "run-agent"
        );
        assert!(
            select_uvx_entry_point("package", &["first".to_string(), "second".to_string()])
                .is_err()
        );
        let hermes_id = AgentId::parse("hermes").unwrap();
        let hermes_entry_point = managed_uvx_entry_point(&hermes_id).unwrap();
        assert_eq!(
            select_uvx_entry_point(
                hermes_entry_point,
                &[
                    "hermes".to_string(),
                    "hermes-agent".to_string(),
                    "hermes-acp".to_string(),
                ],
            )
            .unwrap(),
            "hermes"
        );
        assert!(select_uvx_entry_point("package", &["../escape".to_string()]).is_err());
    }

    #[test]
    fn uvx_launcher_uses_metadata_entry_point_without_shell_interpolation() {
        let launcher = uvx_launcher_source("minion-code", "minion-code").unwrap();
        assert!(launcher.contains("distribution(\"minion-code\")"));
        assert!(launcher.contains("entry.group == \"console_scripts\""));
        assert!(!launcher.contains("subprocess"));
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
    fn pi_npm_lock_verifies_the_adapter_and_coding_agent() {
        let temp = tempfile::tempdir().unwrap();
        let adapter = (
            "pi-acp",
            "0.0.33",
            "sha512-vX9kY1tK14E72G4dBAx+RGCk/k7XPjTHls6dLUxA8WSkBav6B6JHuSBv3eusp50LCR/GTRsR2kIKsG0Z5jANzw==",
        );
        let runtime = (
            PI_CODING_AGENT_PACKAGE,
            "1.2.3",
            "sha512-ncAqFrG+iybuPGOhMiZoEHkEzTpJgz3guYD32pD+M7ucc0WeHmauP6wa7qwP8V/KWvsZDVNa5XGsdZ7fkC7w7A==",
        );
        let adapter_resolved = canonical_npm_tarball_url(adapter.0, adapter.1).unwrap();
        let runtime_resolved = canonical_npm_tarball_url(runtime.0, runtime.1).unwrap();
        write_json_private(
            &temp.path().join("package-lock.json"),
            &serde_json::json!({
                "packages": {
                    "node_modules/pi-acp": {
                        "version": adapter.1,
                        "integrity": adapter.2,
                        "resolved": adapter_resolved,
                    },
                    "node_modules/@earendil-works/pi-coding-agent": {
                        "version": runtime.1,
                        "integrity": runtime.2,
                        "resolved": runtime_resolved,
                    },
                }
            }),
        )
        .unwrap();

        assert!(
            verify_npm_lock(
                temp.path(),
                adapter.0,
                adapter.1,
                adapter.2,
                &adapter_resolved
            )
            .is_ok()
        );
        assert!(
            verify_npm_lock(
                temp.path(),
                runtime.0,
                runtime.1,
                runtime.2,
                &runtime_resolved
            )
            .is_ok()
        );
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
    fn npm_executable_selection_accepts_aliases_for_the_same_path() {
        let metadata = NpmPackageMetadata {
            name: "@tencent-ai/codebuddy-code".to_string(),
            version: "2.106.7".to_string(),
            dist: NpmDist {
                integrity: None,
                tarball: None,
            },
            bin: Some(NpmBin::Multiple(BTreeMap::from([
                ("cbc".to_string(), "bin/codebuddy".to_string()),
                ("codebuddy".to_string(), "bin/codebuddy".to_string()),
            ]))),
        };

        assert_eq!(
            select_npm_bin(&metadata, "@tencent-ai/codebuddy-code").unwrap(),
            "bin/codebuddy"
        );
    }

    #[test]
    fn autohand_cli_selects_the_canonical_command_from_multiple_bins() {
        let metadata = NpmPackageMetadata {
            name: AUTOHAND_CLI_PACKAGE.to_string(),
            version: "0.9.4".to_string(),
            dist: NpmDist {
                integrity: None,
                tarball: None,
            },
            bin: Some(NpmBin::Multiple(BTreeMap::from([
                ("agent".to_string(), "dist/agent.js".to_string()),
                ("autohand".to_string(), "dist/index.js".to_string()),
                (
                    "autohand-code".to_string(),
                    "dist/autohand-code.js".to_string(),
                ),
            ]))),
        };

        assert_eq!(
            select_npm_bin(&metadata, AUTOHAND_CLI_PACKAGE).unwrap(),
            "dist/index.js"
        );
    }

    #[cfg(unix)]
    #[test]
    fn uvx_cached_installation_recovers_a_relocatable_python_launcher() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let service_root = temp.path().join("managed-agents");
        let python = service_root.join("runtimes/python/cpython/bin/python");
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        write_version_probe(&python, "3.12.13");

        let install_root = service_root.join("agents/fast-agent/0.9.30-fixture");
        let venv_python = install_root.join("venv/bin/python");
        fs::create_dir_all(venv_python.parent().unwrap()).unwrap();
        symlink(&python, &venv_python).unwrap();
        let launcher = install_root.join("vibex-uvx-launcher.py");
        fs::write(&launcher, b"raise SystemExit(0)\n").unwrap();
        let manifest = InstallManifest {
            registry_agent_id: "fast-agent".to_string(),
            version: "0.9.30".to_string(),
            fingerprint: "fixture".to_string(),
            runtime_version: None,
            distribution_kind: AgentManagedDistributionKind::Uvx,
            launch: ManifestLaunch::Python {
                python: "venv/bin/python".to_string(),
                script: "vibex-uvx-launcher.py".to_string(),
                args: vec!["-x".to_string()],
            },
        };
        write_json_private(&install_root.join("vibex-install.json"), &manifest).unwrap();

        let installed = load_installed_agent(&install_root, "fixture").unwrap();
        assert_eq!(installed.kind, AgentManagedDistributionKind::Uvx);
        assert_eq!(
            installed.command.command,
            venv_python.to_string_lossy().into_owned()
        );
        assert_eq!(
            installed.command.args,
            vec![launcher.to_string_lossy().into_owned(), "-x".to_string()]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn uvx_metadata_probe_uses_isolated_python_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let python = temp.path().join("python");
        fs::write(
            &python,
            r#"#!/bin/sh
[ "$1" = "-I" ] || exit 1
[ "$2" = "-c" ] || exit 1
[ "$4" = "test-package" ] || exit 1
printf '%s\n' '{"version":"1.2.3","scripts":["test-package"]}'
"#,
        )
        .unwrap();
        fs::set_permissions(&python, fs::Permissions::from_mode(0o700)).unwrap();

        let package = inspect_uvx_package(&python, "test-package").await.unwrap();
        assert_eq!(package.version, "1.2.3");
        assert_eq!(package.scripts, vec!["test-package"]);
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

    #[test]
    fn legacy_companion_installations_require_private_runtime_repair() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("autohand");
        fs::create_dir_all(&root).unwrap();
        let adapter = root.join("adapter.cjs");
        fs::write(&adapter, b"fixture").unwrap();
        let agent_id = AgentId::parse("autohand").unwrap();
        let record = AgentManagedInstallationRecord {
            agent_id: agent_id.clone(),
            registry_agent_id: "autohand".to_string(),
            state: AgentManagedInstallState {
                managed: true,
                status: AgentManagedInstallStatus::Installed,
                distribution_kind: Some(AgentManagedDistributionKind::Npm),
                installed_version: Some("0.2.1".to_string()),
                available_version: Some("0.2.1".to_string()),
                last_error_code: None,
                last_error_message: None,
                updated_at_ms: Some(1),
            },
            command: Some(AgentCommandConfig {
                command: adapter.to_string_lossy().into_owned(),
                args: Vec::new(),
            }),
            install_root: Some(root.to_string_lossy().into_owned()),
            updated_at_ms: 1,
        };
        assert!(!managed_companion_installation_is_usable(
            &agent_id, &record
        ));

        let companion = npm_command_path(&root, "autohand");
        write_private_file(&companion, b"fixture").unwrap();
        write_json_private(
            &root.join("vibex-install.json"),
            &InstallManifest {
                registry_agent_id: "autohand".to_string(),
                version: "0.2.1".to_string(),
                fingerprint: "fixture".to_string(),
                runtime_version: Some("0.9.4".to_string()),
                distribution_kind: AgentManagedDistributionKind::Npm,
                launch: ManifestLaunch::Node {
                    node: adapter.to_string_lossy().into_owned(),
                    script: "adapter.cjs".to_string(),
                    args: Vec::new(),
                },
            },
        )
        .unwrap();
        assert!(managed_companion_installation_is_usable(&agent_id, &record));
        assert!(load_installed_agent(&root, "fixture").is_ok());
        fs::remove_file(companion).unwrap();
        assert!(load_installed_agent(&root, "fixture").is_err());
    }

    #[test]
    fn pi_install_manifest_requires_runtime_version() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("pi");
        fs::create_dir_all(&root).unwrap();
        let node = root.join("node");
        let adapter = root.join("adapter.cjs");
        fs::write(&node, b"fixture").unwrap();
        fs::write(&adapter, b"fixture").unwrap();
        write_private_file(&npm_command_path(&root, PI_COMMAND_NAME), b"fixture").unwrap();
        write_json_private(
            &root.join("vibex-install.json"),
            &InstallManifest {
                registry_agent_id: "pi-acp".to_string(),
                version: "0.0.33".to_string(),
                fingerprint: "fixture".to_string(),
                runtime_version: None,
                distribution_kind: AgentManagedDistributionKind::Npm,
                launch: ManifestLaunch::Node {
                    node: node.to_string_lossy().into_owned(),
                    script: "adapter.cjs".to_string(),
                    args: Vec::new(),
                },
            },
        )
        .unwrap();

        let error = load_installed_agent(&root, "fixture").unwrap_err();
        assert_eq!(error.code, "agent_npm_runtime_version_missing");
    }

    #[test]
    fn amp_binary_install_manifest_requires_and_accepts_private_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("amp");
        fs::create_dir_all(&root).unwrap();
        let node = root.join("node");
        let launcher = root.join(NPM_COMPANION_LAUNCHER_NAME);
        fs::write(&node, b"fixture").unwrap();
        fs::write(&launcher, b"fixture").unwrap();
        write_private_file(&npm_command_path(&root, "amp"), b"fixture").unwrap();

        let mut manifest = InstallManifest {
            registry_agent_id: "amp-acp".to_string(),
            version: "0.9.0".to_string(),
            fingerprint: "fixture".to_string(),
            runtime_version: None,
            distribution_kind: AgentManagedDistributionKind::Binary,
            launch: ManifestLaunch::Node {
                node: node.to_string_lossy().into_owned(),
                script: NPM_COMPANION_LAUNCHER_NAME.to_string(),
                args: Vec::new(),
            },
        };
        write_json_private(&root.join("vibex-install.json"), &manifest).unwrap();

        let error = load_installed_agent(&root, "fixture").unwrap_err();
        assert_eq!(error.code, "agent_npm_runtime_version_missing");

        manifest.runtime_version = Some("0.0.1".to_string());
        write_json_private(&root.join("vibex-install.json"), &manifest).unwrap();
        assert!(load_installed_agent(&root, "fixture").is_ok());
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
    fn tar_bz2_archives_accept_top_level_dot_directory_entries() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("agent.tar.bz2");
        let archive_file = File::create(&archive_path).unwrap();
        let encoder = bzip2::write::BzEncoder::new(archive_file, bzip2::Compression::best());
        let mut builder = tar::Builder::new(encoder);
        let archive_root = temp.path().join("archive-root");
        fs::create_dir(&archive_root).unwrap();
        builder.append_dir(".", &archive_root).unwrap();
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

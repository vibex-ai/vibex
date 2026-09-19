use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256, Sha512};
use tokio::sync::{Notify, watch};

use crate::manifest::hex_lower;
use crate::{
    AppUpdateError, AppUpdateResult, GitHubReleaseSource, InstallMode, Installation,
    UpdateArtifact, UpdateChannel, UpdateSource, verify_manifest,
};

const STARTUP_CHECK_DELAY: Duration = Duration::from_secs(30);
const STABLE_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const PRERELEASE_CHECK_INTERVAL: Duration = Duration::from_secs(2 * 60 * 60);
/// Notes are decoration; a slow notes request must not stall a check.
const RELEASE_NOTES_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(15 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(60 * 60),
];

#[derive(Debug, Clone)]
pub struct AppUpdateConfig {
    pub current_version: Version,
    pub channel: UpdateChannel,
    pub os: String,
    pub arch: String,
    pub installation: Installation,
    pub updates_dir: PathBuf,
    pub public_key_base64: Option<String>,
    pub automatic_checks: bool,
}

impl AppUpdateConfig {
    pub fn for_current_build(
        current_version: &str,
        channel: UpdateChannel,
        runtime_home: impl AsRef<Path>,
    ) -> AppUpdateResult<Self> {
        let current_version = Version::parse(current_version).map_err(|_| {
            AppUpdateError::new(
                "app_update_current_version_invalid",
                "the installed Vibex version is invalid",
            )
        })?;
        Ok(Self {
            current_version,
            channel,
            os: std::env::consts::OS.to_string(),
            arch: normalized_arch(std::env::consts::ARCH).to_string(),
            installation: Installation::detect(),
            updates_dir: runtime_home.as_ref().join("updates"),
            public_key_base64: option_env!("VIBEX_UPDATE_PUBLIC_KEY").map(str::to_string),
            automatic_checks: true,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckReason {
    Automatic,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateFailure {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl From<&AppUpdateError> for UpdateFailure {
    fn from(error: &AppUpdateError) -> Self {
        Self {
            code: error.code.to_string(),
            message: error.message.clone(),
            retryable: error.retryable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRelease {
    pub version: Version,
    pub tag: String,
    pub published_at: String,
    pub notes_url: url::Url,
    pub artifact: Option<UpdateArtifact>,
    /// Raw Markdown notes published with the release, when it ships any.
    ///
    /// Informational only: the document is not covered by the manifest
    /// signature and never influences update decisions.
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateState {
    Idle,
    Checking,
    Available {
        release: UpdateRelease,
    },
    Downloading {
        release: UpdateRelease,
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    Verifying {
        release: UpdateRelease,
    },
    Staged {
        release: UpdateRelease,
        path: PathBuf,
    },
    Installing {
        release: UpdateRelease,
    },
    RestartRequired {
        release: UpdateRelease,
    },
    Unsupported {
        release: UpdateRelease,
        reason: String,
    },
    Error {
        failure: UpdateFailure,
        release: Option<UpdateRelease>,
    },
}

impl UpdateState {
    pub fn release(&self) -> Option<&UpdateRelease> {
        match self {
            Self::Available { release }
            | Self::Downloading { release, .. }
            | Self::Verifying { release }
            | Self::Staged { release, .. }
            | Self::Installing { release }
            | Self::RestartRequired { release }
            | Self::Unsupported { release, .. } => Some(release),
            Self::Error { release, .. } => release.as_ref(),
            Self::Idle | Self::Checking => None,
        }
    }

    pub fn should_show_update_entry(&self) -> bool {
        self.release().is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSnapshot {
    pub seq: u64,
    pub state: UpdateState,
    pub last_successful_check_ms: Option<i64>,
    pub last_automatic_failure: Option<UpdateFailure>,
    /// Notes published for the installed version, when its release ships any.
    ///
    /// Informational only, exactly like [`UpdateRelease::notes`]: the document
    /// is not covered by the manifest signature. A check that finds no newer
    /// release refreshes it; one that finds an update clears it, because the
    /// release carries its own notes.
    pub current_notes: Option<String>,
}

impl Default for UpdateSnapshot {
    fn default() -> Self {
        Self {
            seq: 0,
            state: UpdateState::Idle,
            last_successful_check_ms: None,
            last_automatic_failure: None,
            current_notes: None,
        }
    }
}

/// One manifest check: the state to publish plus the notes that belong to the
/// installed version when no newer release exists.
struct CheckOutcome {
    state: UpdateState,
    current_notes: Option<String>,
}

impl CheckOutcome {
    const fn new(state: UpdateState) -> Self {
        Self {
            state,
            current_notes: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Check,
    Download,
    Install,
}

struct ServiceInner {
    config: AppUpdateConfig,
    source: Arc<dyn UpdateSource>,
    snapshot: Mutex<UpdateSnapshot>,
    snapshot_tx: watch::Sender<UpdateSnapshot>,
    operation: tokio::sync::Mutex<Option<Operation>>,
    operation_notify: Notify,
}

#[derive(Clone)]
pub struct AppUpdateService {
    inner: Arc<ServiceInner>,
}

impl AppUpdateService {
    pub fn new(config: AppUpdateConfig) -> AppUpdateResult<Self> {
        let source = Arc::new(GitHubReleaseSource::official()?);
        Self::with_source(config, source)
    }

    pub fn with_source(
        config: AppUpdateConfig,
        source: Arc<dyn UpdateSource>,
    ) -> AppUpdateResult<Self> {
        fs::create_dir_all(&config.updates_dir).map_err(|_| {
            AppUpdateError::new(
                "app_update_directory_create_failed",
                "the update directory could not be created",
            )
        })?;
        if let Err(error) = config
            .installation
            .confirm_current_version(&config.current_version.to_string(), &config.updates_dir)
        {
            tracing_warning(&error);
        }
        let snapshot = UpdateSnapshot::default();
        let (snapshot_tx, _) = watch::channel(snapshot.clone());
        Ok(Self {
            inner: Arc::new(ServiceInner {
                config,
                source,
                snapshot: Mutex::new(snapshot),
                snapshot_tx,
                operation: tokio::sync::Mutex::new(None),
                operation_notify: Notify::new(),
            }),
        })
    }

    pub fn unavailable(mut config: AppUpdateConfig, error: AppUpdateError) -> Self {
        config.automatic_checks = false;
        let snapshot = UpdateSnapshot {
            seq: 1,
            state: UpdateState::Error {
                failure: UpdateFailure::from(&error),
                release: None,
            },
            last_successful_check_ms: None,
            last_automatic_failure: None,
            current_notes: None,
        };
        let (snapshot_tx, _) = watch::channel(snapshot.clone());
        Self {
            inner: Arc::new(ServiceInner {
                config,
                source: Arc::new(UnavailableSource { error }),
                snapshot: Mutex::new(snapshot),
                snapshot_tx,
                operation: tokio::sync::Mutex::new(None),
                operation_notify: Notify::new(),
            }),
        }
    }

    pub fn snapshot(&self) -> UpdateSnapshot {
        self.inner
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<UpdateSnapshot> {
        self.inner.snapshot_tx.subscribe()
    }

    pub fn automatic_checks_enabled(&self) -> bool {
        self.inner.config.automatic_checks && self.inner.config.public_key_base64.is_some()
    }

    pub async fn run_automatic_checks(self) {
        if !self.automatic_checks_enabled() {
            return;
        }
        tokio::time::sleep(STARTUP_CHECK_DELAY).await;
        let mut retry_index = 0usize;
        loop {
            let result = self.check(CheckReason::Automatic).await;
            let delay = if result.is_ok() {
                retry_index = 0;
                jittered(self.check_interval(), &self.inner.config.current_version)
            } else {
                let delay = RETRY_DELAYS
                    .get(retry_index)
                    .copied()
                    .unwrap_or(STABLE_CHECK_INTERVAL);
                retry_index = retry_index.saturating_add(1);
                delay
            };
            tokio::time::sleep(delay).await;
        }
    }

    pub async fn check(&self, reason: CheckReason) -> AppUpdateResult<UpdateSnapshot> {
        if reason == CheckReason::Automatic
            && matches!(
                self.snapshot().state,
                UpdateState::Downloading { .. }
                    | UpdateState::Verifying { .. }
                    | UpdateState::Staged { .. }
                    | UpdateState::Installing { .. }
                    | UpdateState::RestartRequired { .. }
            )
        {
            return Ok(self.snapshot());
        }
        if !self.claim_or_wait(Operation::Check).await {
            return Ok(self.snapshot());
        }
        let previous_state = self.snapshot().state;
        if reason == CheckReason::Manual {
            self.publish_state(UpdateState::Checking);
        }
        let result = self.check_inner().await;
        match &result {
            Ok(outcome) => {
                self.publish_successful_check(outcome.state.clone(), outcome.current_notes.clone());
            }
            Err(error) if reason == CheckReason::Automatic => {
                self.publish_automatic_failure(error, previous_state);
            }
            Err(error) => {
                self.publish_state(UpdateState::Error {
                    failure: UpdateFailure::from(error),
                    release: previous_state.release().cloned(),
                });
            }
        }
        self.release_operation().await;
        result.map(|_| self.snapshot())
    }

    pub async fn download(&self) -> AppUpdateResult<UpdateSnapshot> {
        if !self.claim_or_wait(Operation::Download).await {
            return Ok(self.snapshot());
        }
        let result = self.download_inner().await;
        if let Err(error) = &result {
            let release = self.snapshot().state.release().cloned();
            self.publish_state(UpdateState::Error {
                failure: UpdateFailure::from(error),
                release,
            });
        }
        self.release_operation().await;
        result.map(|_| self.snapshot())
    }

    pub async fn install(&self) -> AppUpdateResult<UpdateSnapshot> {
        if !self.claim_or_wait(Operation::Install).await {
            return Ok(self.snapshot());
        }
        let result = self.install_inner().await;
        if let Err(error) = &result {
            let release = self.snapshot().state.release().cloned();
            self.publish_state(UpdateState::Error {
                failure: UpdateFailure::from(error),
                release,
            });
        }
        self.release_operation().await;
        result.map(|_| self.snapshot())
    }

    pub fn restart(&self) -> AppUpdateResult<()> {
        if !matches!(self.snapshot().state, UpdateState::RestartRequired { .. }) {
            return Err(AppUpdateError::new(
                "app_update_restart_not_ready",
                "the update is not ready to restart",
            ));
        }
        self.inner.config.installation.restart()
    }

    async fn check_inner(&self) -> AppUpdateResult<CheckOutcome> {
        let public_key = self
            .inner
            .config
            .public_key_base64
            .as_deref()
            .ok_or_else(|| {
                AppUpdateError::new(
                    "app_update_verification_key_unavailable",
                    "this Vibex build does not contain an update verification key",
                )
            })?;
        let Some(signed) = self
            .inner
            .source
            .latest_signed_manifest(self.inner.config.channel)
            .await?
        else {
            return Ok(CheckOutcome {
                state: UpdateState::Idle,
                current_notes: self.current_version_notes().await,
            });
        };
        let verified = verify_manifest(
            &signed.manifest,
            &signed.signature_base64,
            public_key,
            self.inner.config.channel,
            &signed.tag,
        )?;
        if verified.manifest.version <= self.inner.config.current_version {
            return Ok(CheckOutcome {
                state: UpdateState::Idle,
                current_notes: self.current_version_notes().await,
            });
        }
        let artifact = verified
            .matching_artifact(
                &self.inner.config.os,
                &self.inner.config.arch,
                &self.inner.config.installation.package,
            )
            .cloned();
        let mut release = UpdateRelease {
            version: verified.manifest.version,
            tag: verified.manifest.tag,
            published_at: verified.manifest.published_at,
            notes_url: verified.manifest.notes_url,
            artifact,
            notes: None,
        };
        release.notes = self.release_notes(&release.tag).await;
        let Some(install_mode) = release
            .artifact
            .as_ref()
            .map(|artifact| artifact.install_mode)
        else {
            return Ok(CheckOutcome::new(UpdateState::Unsupported {
                release,
                reason: "No signed update package matches this operating system, architecture, and installation source."
                    .to_string(),
            }));
        };
        if !self.inner.config.installation.supports(install_mode) {
            return Ok(CheckOutcome::new(UpdateState::Unsupported {
                release,
                reason: external_install_reason(install_mode).to_string(),
            }));
        }
        Ok(CheckOutcome::new(UpdateState::Available { release }))
    }

    /// Notes for the installed version, so About can show a changelog even when
    /// nothing newer exists.
    ///
    /// Notes for a published tag never change, so the first successful fetch is
    /// reused for the rest of the process; a missing document is retried by the
    /// next check rather than cached as absent.
    async fn current_version_notes(&self) -> Option<String> {
        if let Some(notes) = self.snapshot().current_notes {
            return Some(notes);
        }
        let tag = format!("v{}", self.inner.config.current_version);
        self.release_notes(&tag).await
    }

    /// Fetch informational notes without letting them fail or stall a check.
    ///
    /// The signed manifest stays the only source of update truth, so a missing
    /// document, a transport error, or a slow response simply leaves the About
    /// page without notes.
    async fn release_notes(&self, tag: &str) -> Option<String> {
        match tokio::time::timeout(RELEASE_NOTES_TIMEOUT, self.inner.source.release_notes(tag))
            .await
        {
            Ok(Ok(notes)) => notes,
            Ok(Err(_)) | Err(_) => None,
        }
    }

    async fn download_inner(&self) -> AppUpdateResult<()> {
        let release = match self.snapshot().state {
            UpdateState::Available { release }
            | UpdateState::Error {
                release: Some(release),
                ..
            } => release,
            UpdateState::Staged { .. } | UpdateState::RestartRequired { .. } => return Ok(()),
            _ => {
                return Err(AppUpdateError::new(
                    "app_update_download_not_available",
                    "no installable update is available",
                ));
            }
        };
        let artifact = release.artifact.clone().ok_or_else(|| {
            AppUpdateError::new(
                "app_update_artifact_unavailable",
                "no signed update package matches this installation",
            )
        })?;
        let file_name = artifact_file_name(&artifact)?;
        let version_dir = self
            .inner
            .config
            .updates_dir
            .join(release.version.to_string());
        tokio::fs::create_dir_all(&version_dir).await.map_err(|_| {
            AppUpdateError::new(
                "app_update_download_directory_create_failed",
                "the update download directory could not be created",
            )
        })?;
        let final_path = version_dir.join(file_name);
        if verify_artifact(&final_path, &artifact).await.is_ok() {
            self.publish_state(UpdateState::Staged {
                release,
                path: final_path,
            });
            return Ok(());
        }
        let temporary = final_path.with_extension("part");
        let _ = tokio::fs::remove_file(&temporary).await;
        self.publish_state(UpdateState::Downloading {
            release: release.clone(),
            downloaded_bytes: 0,
            total_bytes: artifact.size,
        });
        let service = self.clone();
        let progress_release = release.clone();
        let progress_state = Arc::new(Mutex::new((Instant::now(), 0u64)));
        let progress = Arc::new(move |downloaded_bytes: u64, total_bytes: u64| {
            let mut last = progress_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let now = Instant::now();
            if downloaded_bytes != total_bytes
                && downloaded_bytes.saturating_sub(last.1) < 1024 * 1024
                && now.duration_since(last.0) < Duration::from_millis(100)
            {
                return;
            }
            *last = (now, downloaded_bytes);
            service.publish_state(UpdateState::Downloading {
                release: progress_release.clone(),
                downloaded_bytes,
                total_bytes,
            });
        });
        if let Err(error) = self
            .inner
            .source
            .download_artifact(&artifact, &temporary, progress)
            .await
        {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error);
        }
        self.publish_state(UpdateState::Verifying {
            release: release.clone(),
        });
        if let Err(error) = verify_artifact(&temporary, &artifact).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error);
        }
        let _ = tokio::fs::remove_file(&final_path).await;
        tokio::fs::rename(&temporary, &final_path)
            .await
            .map_err(|_| {
                AppUpdateError::new(
                    "app_update_download_commit_failed",
                    "the verified update could not be staged",
                )
            })?;
        self.publish_state(UpdateState::Staged {
            release,
            path: final_path,
        });
        Ok(())
    }

    async fn install_inner(&self) -> AppUpdateResult<()> {
        let (release, path) = match self.snapshot().state {
            UpdateState::Staged { release, path } => (release, path),
            UpdateState::RestartRequired { .. } => return Ok(()),
            _ => {
                return Err(AppUpdateError::new(
                    "app_update_install_not_staged",
                    "the update has not been downloaded and verified",
                ));
            }
        };
        let artifact = release.artifact.clone().ok_or_else(|| {
            AppUpdateError::new(
                "app_update_artifact_unavailable",
                "no signed update package matches this installation",
            )
        })?;
        verify_artifact(&path, &artifact).await?;
        self.publish_state(UpdateState::Installing {
            release: release.clone(),
        });
        let installation = self.inner.config.installation.clone();
        let updates_dir = self.inner.config.updates_dir.clone();
        let version = release.version.to_string();
        let outcome = tokio::task::spawn_blocking(move || {
            installation.install(&artifact, &path, &version, &updates_dir)
        })
        .await
        .map_err(|_| {
            AppUpdateError::new(
                "app_update_install_task_failed",
                "the update installer stopped unexpectedly",
            )
        })??;
        match outcome {
            crate::install::InstallOutcome::RestartRequired => {
                self.publish_state(UpdateState::RestartRequired { release });
            }
            crate::install::InstallOutcome::InstallerLaunched => {
                self.publish_state(UpdateState::Installing { release });
            }
        }
        Ok(())
    }

    async fn claim_or_wait(&self, operation: Operation) -> bool {
        loop {
            let notified = self.inner.operation_notify.notified();
            {
                let mut current = self.inner.operation.lock().await;
                match *current {
                    None => {
                        *current = Some(operation);
                        return true;
                    }
                    Some(active) if active != operation => return false,
                    Some(_) => {}
                }
            }
            notified.await;
            if self.inner.operation.lock().await.is_none() {
                return false;
            }
        }
    }

    async fn release_operation(&self) {
        *self.inner.operation.lock().await = None;
        self.inner.operation_notify.notify_waiters();
    }

    fn publish_state(&self, state: UpdateState) {
        self.update_snapshot(|snapshot| snapshot.state = state);
    }

    fn publish_successful_check(&self, state: UpdateState, current_notes: Option<String>) {
        self.update_snapshot(|snapshot| {
            snapshot.state = state;
            snapshot.current_notes = current_notes;
            snapshot.last_successful_check_ms = Some(now_ms());
            snapshot.last_automatic_failure = None;
        });
    }

    fn publish_automatic_failure(&self, error: &AppUpdateError, previous_state: UpdateState) {
        self.update_snapshot(|snapshot| {
            snapshot.last_automatic_failure = Some(UpdateFailure::from(error));
            snapshot.state = previous_state;
        });
    }

    fn update_snapshot(&self, update: impl FnOnce(&mut UpdateSnapshot)) {
        let next = {
            let mut snapshot = self
                .inner
                .snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            update(&mut snapshot);
            snapshot.seq = snapshot.seq.saturating_add(1);
            snapshot.clone()
        };
        let _ = self.inner.snapshot_tx.send(next);
    }

    fn check_interval(&self) -> Duration {
        match self.inner.config.channel {
            UpdateChannel::Stable => STABLE_CHECK_INTERVAL,
            UpdateChannel::Rc | UpdateChannel::Preview => PRERELEASE_CHECK_INTERVAL,
        }
    }
}

struct UnavailableSource {
    error: AppUpdateError,
}

#[async_trait]
impl UpdateSource for UnavailableSource {
    async fn latest_signed_manifest(
        &self,
        _channel: UpdateChannel,
    ) -> AppUpdateResult<Option<crate::SignedManifest>> {
        Err(self.error.clone())
    }

    async fn release_notes(&self, _tag: &str) -> AppUpdateResult<Option<String>> {
        Err(self.error.clone())
    }

    async fn download_artifact(
        &self,
        _artifact: &UpdateArtifact,
        _destination: &Path,
        _progress: crate::UpdateDownloadProgress,
    ) -> AppUpdateResult<()> {
        Err(self.error.clone())
    }
}

async fn verify_artifact(path: &Path, artifact: &UpdateArtifact) -> AppUpdateResult<()> {
    let mut file = tokio::fs::File::open(path).await.map_err(|_| {
        AppUpdateError::new(
            "app_update_artifact_read_failed",
            "the downloaded update could not be read",
        )
    })?;
    let metadata = file.metadata().await.map_err(|_| {
        AppUpdateError::new(
            "app_update_artifact_read_failed",
            "the downloaded update metadata could not be read",
        )
    })?;
    if metadata.len() != artifact.size {
        return Err(AppUpdateError::new(
            "app_update_artifact_size_mismatch",
            "the downloaded update size does not match the signed manifest",
        ));
    }
    let mut sha256 = Sha256::new();
    let mut sha512 = artifact.sha512.as_ref().map(|_| Sha512::new());
    let mut buffer = vec![0u8; 128 * 1024];
    loop {
        use tokio::io::AsyncReadExt as _;
        let count = file.read(&mut buffer).await.map_err(|_| {
            AppUpdateError::new(
                "app_update_artifact_read_failed",
                "the downloaded update could not be verified",
            )
        })?;
        if count == 0 {
            break;
        }
        sha256.update(&buffer[..count]);
        if let Some(sha512) = sha512.as_mut() {
            sha512.update(&buffer[..count]);
        }
    }
    if hex_lower(&sha256.finalize()) != artifact.sha256
        || sha512
            .map(|digest| hex_lower(&digest.finalize()))
            .zip(artifact.sha512.as_ref())
            .is_some_and(|(actual, expected)| &actual != expected)
    {
        return Err(AppUpdateError::new(
            "app_update_artifact_hash_mismatch",
            "the downloaded update failed signed hash verification",
        ));
    }
    Ok(())
}

fn artifact_file_name(artifact: &UpdateArtifact) -> AppUpdateResult<&str> {
    artifact
        .url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 180
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
        .ok_or_else(|| {
            AppUpdateError::new(
                "app_update_artifact_name_invalid",
                "the update artifact file name is invalid",
            )
        })
}

fn external_install_reason(mode: InstallMode) -> &'static str {
    match mode {
        InstallMode::Store => "This Vibex installation is updated by its application store.",
        InstallMode::External => {
            "This Vibex installation is updated by its package manager or distribution source."
        }
        InstallMode::SelfReplace | InstallMode::SystemInstaller => {
            "This update package cannot replace the current installation."
        }
    }
}

fn normalized_arch(arch: &str) -> &str {
    match arch {
        "aarch64" => "arm64",
        other => other,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn jittered(base: Duration, version: &Version) -> Duration {
    let spread = base.as_secs() / 20;
    if spread == 0 {
        return base;
    }
    let seed = version
        .to_string()
        .bytes()
        .fold(now_ms() as u64, |seed, byte| {
            seed.rotate_left(5) ^ u64::from(byte)
        });
    let offset = seed % (spread * 2 + 1);
    Duration::from_secs(base.as_secs() - spread + offset)
}

fn tracing_warning(error: &AppUpdateError) {
    tracing::warn!(
        target: "vibex_app_update",
        error_code = error.code,
        "Previous update cleanup failed"
    );
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
    use ed25519_dalek::{Signer as _, SigningKey};
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::{SignedManifest, UpdateDownloadProgress};

    struct FakeSource {
        signed: SignedManifest,
        artifact_bytes: Vec<u8>,
        notes: AppUpdateResult<Option<String>>,
    }

    #[async_trait]
    impl UpdateSource for FakeSource {
        async fn latest_signed_manifest(
            &self,
            _channel: UpdateChannel,
        ) -> AppUpdateResult<Option<SignedManifest>> {
            Ok(Some(self.signed.clone()))
        }

        async fn release_notes(&self, _tag: &str) -> AppUpdateResult<Option<String>> {
            self.notes.clone()
        }

        async fn download_artifact(
            &self,
            artifact: &UpdateArtifact,
            destination: &Path,
            progress: UpdateDownloadProgress,
        ) -> AppUpdateResult<()> {
            tokio::fs::write(destination, &self.artifact_bytes)
                .await
                .unwrap();
            progress(artifact.size, artifact.size);
            Ok(())
        }
    }

    fn fixture() -> (AppUpdateConfig, Arc<dyn UpdateSource>) {
        fixture_for(
            "0.2.0",
            "0.1.0",
            Ok(Some(
                "## English\n\n### Highlights\n\n- A verified change.\n".to_string(),
            )),
        )
    }

    fn fixture_for(
        manifest_version: &str,
        current_version: &str,
        notes: AppUpdateResult<Option<String>>,
    ) -> (AppUpdateConfig, Arc<dyn UpdateSource>) {
        let directory = tempdir().unwrap().keep();
        let bytes = b"verified package".to_vec();
        let sha256 = hex_lower(&Sha256::digest(&bytes));
        let tag = format!("v{manifest_version}");
        let manifest = json!({
            "schema": 1,
            "channel": "stable",
            "version": manifest_version,
            "tag": tag,
            "published_at": "2026-08-16T00:00:00Z",
            "minimum_updater_version": "1",
            "notes_url": format!("https://github.com/vibex-ai/vibex/releases/tag/{tag}"),
            "artifacts": [{
                "os": "linux",
                "arch": "x86_64",
                "package": "deb",
                "install_mode": "system_installer",
                "url": format!("https://github.com/vibex-ai/vibex/releases/download/{tag}/vibex-{manifest_version}-linux-x86_64-deb.deb"),
                "size": bytes.len(),
                "sha256": sha256
            }]
        });
        let raw = serde_json::to_vec(&manifest).unwrap();
        let key = SigningKey::from_bytes(&[4; 32]);
        let signature = BASE64_STANDARD.encode(key.sign(&raw).to_bytes());
        let config = AppUpdateConfig {
            current_version: Version::parse(current_version).unwrap(),
            channel: UpdateChannel::Stable,
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            installation: Installation {
                kind: crate::InstallationKind::Deb,
                package: "deb".to_string(),
                target_path: None,
            },
            updates_dir: directory,
            public_key_base64: Some(BASE64_STANDARD.encode(key.verifying_key().to_bytes())),
            automatic_checks: false,
        };
        let source: Arc<dyn UpdateSource> = Arc::new(FakeSource {
            signed: SignedManifest {
                tag,
                manifest: raw,
                signature_base64: signature,
            },
            artifact_bytes: bytes,
            notes,
        });
        (config, source)
    }

    fn release_of(state: &UpdateState) -> &UpdateRelease {
        state.release().expect("state should carry a release")
    }

    #[tokio::test]
    async fn check_download_and_verify_publish_monotonic_snapshots() {
        let (config, source) = fixture();
        let service = AppUpdateService::with_source(config, source).unwrap();
        let checked = service.check(CheckReason::Manual).await.unwrap();
        assert!(matches!(checked.state, UpdateState::Available { .. }));
        let staged = service.download().await.unwrap();
        assert!(matches!(staged.state, UpdateState::Staged { .. }));
        assert!(staged.seq > checked.seq);
    }

    #[tokio::test]
    async fn check_attaches_published_release_notes() {
        let (config, source) = fixture();
        let service = AppUpdateService::with_source(config, source).unwrap();
        let checked = service.check(CheckReason::Manual).await.unwrap();
        let notes = release_of(&checked.state)
            .notes
            .as_deref()
            .expect("a published document should reach the release");
        assert!(notes.contains("A verified change."));
        assert_eq!(
            checked.current_notes, None,
            "an available release must not keep the installed version's notes"
        );
    }

    #[tokio::test]
    async fn up_to_date_check_attaches_the_installed_version_notes() {
        let (config, source) = fixture_for(
            "0.1.0",
            "0.1.0",
            Ok(Some(
                "## English\n\n### Fixes\n\n- A shipped fix.\n".to_string(),
            )),
        );
        let service = AppUpdateService::with_source(config, source).unwrap();

        let checked = service.check(CheckReason::Manual).await.unwrap();

        assert!(matches!(checked.state, UpdateState::Idle));
        let notes = checked
            .current_notes
            .as_deref()
            .expect("the installed version's notes should reach the snapshot");
        assert!(notes.contains("A shipped fix."));
    }

    #[tokio::test]
    async fn up_to_date_check_survives_missing_or_failed_installed_notes() {
        for notes in [
            Ok(None),
            Err(AppUpdateError::new(
                "app_update_network_failed",
                "the update service could not be reached",
            )),
        ] {
            let (config, source) = fixture_for("0.1.0", "0.1.0", notes);
            let service = AppUpdateService::with_source(config, source).unwrap();

            let checked = service.check(CheckReason::Manual).await.unwrap();

            assert!(matches!(checked.state, UpdateState::Idle));
            assert_eq!(checked.current_notes, None);
        }
    }

    #[tokio::test]
    async fn notes_failure_never_fails_or_blocks_a_check() {
        for notes in [
            Ok(None),
            Err(AppUpdateError::new(
                "app_update_network_failed",
                "the update service could not be reached",
            )),
        ] {
            let (config, source) = fixture();
            let signed = source
                .latest_signed_manifest(UpdateChannel::Stable)
                .await
                .unwrap()
                .unwrap();
            let source: Arc<dyn UpdateSource> = Arc::new(FakeSource {
                signed,
                artifact_bytes: b"verified package".to_vec(),
                notes,
            });
            let service = AppUpdateService::with_source(config, source).unwrap();
            let checked = service.check(CheckReason::Manual).await.unwrap();
            assert!(matches!(checked.state, UpdateState::Available { .. }));
            assert_eq!(release_of(&checked.state).notes, None);
        }
    }

    #[tokio::test]
    async fn modified_download_is_rejected_and_not_staged() {
        let (config, source) = fixture();
        let bad_source: Arc<dyn UpdateSource> = Arc::new(FakeSource {
            signed: source
                .latest_signed_manifest(UpdateChannel::Stable)
                .await
                .unwrap()
                .unwrap(),
            artifact_bytes: b"tampered package".to_vec(),
            notes: Ok(None),
        });
        let service = AppUpdateService::with_source(config, bad_source).unwrap();
        service.check(CheckReason::Manual).await.unwrap();
        let error = service.download().await.unwrap_err();
        assert_eq!(error.code, "app_update_artifact_hash_mismatch");
        assert!(matches!(
            service.snapshot().state,
            UpdateState::Error { .. }
        ));
    }

    #[tokio::test]
    async fn automatic_check_does_not_replace_a_staged_update() {
        let (config, source) = fixture();
        let service = AppUpdateService::with_source(config, source).unwrap();
        service.check(CheckReason::Manual).await.unwrap();
        let staged = service.download().await.unwrap();

        let checked = service.check(CheckReason::Automatic).await.unwrap();

        assert_eq!(checked, staged);
        assert!(matches!(checked.state, UpdateState::Staged { .. }));
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use vibex_core::{AgentId, SessionRuntimeSelection};

use crate::{NewSessionLocation, SidebarHierarchyMode};

pub const DESKTOP_UI_STATE_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_UI_STATE_WRITE_DELAY_MS: u64 = 200;
pub const MIN_UI_STATE_WRITE_DELAY_MS: u64 = 100;
pub const MAX_UI_STATE_WRITE_DELAY_MS: u64 = 300;
pub const DEFAULT_CORRUPT_BACKUP_LIMIT: usize = 3;
pub const RUNTIME_SELECTION_PREFERENCE_LIMIT: usize = 256;

#[derive(Debug, Error)]
pub enum UiStateError {
    #[error("desktop UI state I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("desktop UI state JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("desktop UI state schema version {0} is unsupported")]
    UnsupportedVersion(u64),
    #[error("desktop UI state failed validation: {0}")]
    Validation(&'static str),
}

impl UiStateError {
    /// Return a bounded, stable category for command/diagnostic boundaries.
    /// The display string can contain paths, schema text, or user-controlled
    /// keys and must never be copied into release diagnostics.
    pub fn stable_code(&self) -> &'static str {
        match self {
            Self::Io(_) => "storage/desktop_ui_state_io",
            Self::Json(_) => "validation/desktop_ui_state_json_invalid",
            Self::UnsupportedVersion(_) => "validation/desktop_ui_state_version_unsupported",
            Self::Validation(_) => "validation/desktop_ui_state_invalid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Light,
    Dark,
    #[default]
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LocaleMode {
    En,
    ZhCn,
    ZhTw,
    #[default]
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontSetting {
    pub family: Option<String>,
    pub size: u16,
    pub weight: u16,
}

impl FontSetting {
    fn interface_default() -> Self {
        Self {
            family: Some("Inter Variable".to_string()),
            size: 14,
            weight: 400,
        }
    }

    fn code_default() -> Self {
        Self {
            family: None,
            size: 13,
            weight: 400,
        }
    }

    fn normalize(&mut self, minimum_size: u16) {
        self.family = self
            .family
            .take()
            .map(|family| family.trim().chars().take(160).collect::<String>())
            .filter(|family| !family.is_empty());
        self.size = self.size.clamp(minimum_size, 24);
        self.weight = self.weight.clamp(100, 900);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceUiState {
    pub theme: ThemeMode,
    pub locale: LocaleMode,
    pub interface_font: FontSetting,
    pub code_font: FontSetting,
    pub reduced_motion: bool,
    pub high_contrast: bool,
}

impl Default for AppearanceUiState {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            locale: LocaleMode::System,
            interface_font: FontSetting::interface_default(),
            code_font: FontSetting::code_default(),
            reduced_motion: false,
            high_contrast: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchUiState {
    pub active_tab: String,
    pub selected_workspace_id: Option<String>,
    pub selected_session_id: Option<String>,
    #[serde(default)]
    pub selected_file_path: Option<String>,
    #[serde(default)]
    pub selected_git_path: Option<String>,
    pub sidebar_visible: bool,
    pub preview_visible: bool,
    pub right_rail_visible: bool,
    pub sidebar_width: f32,
    pub preview_width: f32,
    pub right_rail_width: f32,
}

impl Default for WorkbenchUiState {
    fn default() -> Self {
        Self {
            active_tab: "agent".to_string(),
            selected_workspace_id: None,
            selected_session_id: None,
            selected_file_path: None,
            selected_git_path: None,
            sidebar_visible: true,
            preview_visible: false,
            right_rail_visible: false,
            sidebar_width: 320.0,
            preview_width: 520.0,
            right_rail_width: 336.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SidebarUiState {
    pub project_order: Vec<String>,
    pub session_order: Vec<String>,
    pub pinned_session_ids: BTreeSet<String>,
    pub collapsed_project_ids: BTreeSet<String>,
    #[serde(default)]
    pub collapsed_workspace_ids: BTreeSet<String>,
    #[serde(default)]
    pub hierarchy_mode: SidebarHierarchyMode,
    #[serde(default)]
    pub project_location_preferences: BTreeMap<String, NewSessionLocation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewUiState {
    pub focused_pane_id: Option<String>,
    pub pinned_tab_ids: Vec<String>,
    pub split_sizes: Vec<f32>,
    #[serde(default)]
    pub layout: crate::PreviewState,
    #[serde(default)]
    pub editor_recovery: crate::EditorRecoverySnapshot,
}

impl Default for PreviewUiState {
    fn default() -> Self {
        Self {
            focused_pane_id: None,
            pinned_tab_ids: Vec::new(),
            split_sizes: vec![1.0],
            layout: crate::PreviewState::default(),
            editor_recovery: crate::EditorRecoverySnapshot::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TerminalUiState {
    pub tab_order: Vec<String>,
    pub selected_terminal_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RightRailUiState {
    pub activity_order: Vec<String>,
    pub selected_activity_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionContentWidthMode {
    Narrow,
    #[default]
    Standard,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComposerUiState {
    pub terminal_ids: Vec<String>,
    #[serde(default)]
    pub runtime_selections_by_agent: BTreeMap<AgentId, SessionRuntimeSelection>,
    #[serde(default)]
    pub runtime_selections_by_model: Vec<SessionRuntimeSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUiState {
    pub content_width: SessionContentWidthMode,
    pub turn_preview_rail: bool,
    #[serde(default = "default_enhanced_command_execution_display")]
    pub enhanced_command_execution_display: bool,
}

impl Default for SessionUiState {
    fn default() -> Self {
        Self {
            content_width: SessionContentWidthMode::Standard,
            turn_preview_rail: true,
            enhanced_command_execution_display: true,
        }
    }
}

const fn default_enhanced_command_execution_display() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UiStateMigration {
    pub source_schema: Option<String>,
    #[serde(default)]
    pub migrated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopUiStateV1 {
    pub schema_version: u32,
    pub source_app_version: String,
    pub appearance: AppearanceUiState,
    pub workbench: WorkbenchUiState,
    pub sidebar: SidebarUiState,
    pub preview: PreviewUiState,
    pub terminal: TerminalUiState,
    pub right_rail: RightRailUiState,
    #[serde(default)]
    pub session: SessionUiState,
    #[serde(default)]
    pub composer: ComposerUiState,
    #[serde(default)]
    pub terminal_tab_titles: BTreeMap<String, String>,
    #[serde(default)]
    pub agent_tab_order: Vec<String>,
    #[serde(default)]
    pub plugin_order_migrated: bool,
    pub migration: UiStateMigration,
}

impl Default for DesktopUiStateV1 {
    fn default() -> Self {
        Self {
            schema_version: DESKTOP_UI_STATE_SCHEMA_VERSION,
            source_app_version: env!("CARGO_PKG_VERSION").to_string(),
            appearance: AppearanceUiState::default(),
            workbench: WorkbenchUiState::default(),
            sidebar: SidebarUiState::default(),
            preview: PreviewUiState::default(),
            terminal: TerminalUiState::default(),
            right_rail: RightRailUiState::default(),
            session: SessionUiState::default(),
            composer: ComposerUiState::default(),
            terminal_tab_titles: BTreeMap::new(),
            agent_tab_order: Vec::new(),
            plugin_order_migrated: false,
            migration: UiStateMigration::default(),
        }
    }
}

impl DesktopUiStateV1 {
    pub fn normalize(&mut self) -> Result<(), UiStateError> {
        if self.schema_version != DESKTOP_UI_STATE_SCHEMA_VERSION {
            return Err(UiStateError::UnsupportedVersion(self.schema_version.into()));
        }
        self.source_app_version = bounded_required(&self.source_app_version, 80)
            .ok_or(UiStateError::Validation("source app version is empty"))?;
        self.appearance.interface_font.normalize(12);
        self.appearance.code_font.normalize(10);
        self.workbench.active_tab =
            bounded_required(&self.workbench.active_tab, 80).unwrap_or_else(|| "agent".to_string());
        self.workbench.selected_workspace_id =
            bounded_optional(self.workbench.selected_workspace_id.take(), 256);
        self.workbench.selected_session_id =
            bounded_optional(self.workbench.selected_session_id.take(), 256);
        self.workbench.selected_file_path =
            bounded_optional(self.workbench.selected_file_path.take(), 4_096);
        self.workbench.selected_git_path =
            bounded_optional(self.workbench.selected_git_path.take(), 4_096);
        self.workbench.sidebar_width =
            bounded_f32(self.workbench.sidebar_width, 256.0, 480.0, 320.0);
        self.workbench.preview_width =
            bounded_f32(self.workbench.preview_width, 360.0, 900.0, 520.0);
        self.workbench.right_rail_width =
            bounded_f32(self.workbench.right_rail_width, 224.0, 720.0, 336.0);

        normalize_ids(&mut self.sidebar.project_order, 1_000);
        normalize_ids(&mut self.sidebar.session_order, 2_000);
        normalize_set(&mut self.sidebar.pinned_session_ids, 2_000);
        normalize_set(&mut self.sidebar.collapsed_project_ids, 1_000);
        normalize_set(&mut self.sidebar.collapsed_workspace_ids, 2_000);
        self.sidebar.project_location_preferences =
            std::mem::take(&mut self.sidebar.project_location_preferences)
                .into_iter()
                .filter_map(|(project_id, preference)| {
                    bounded_required(&project_id, 256).map(|project_id| (project_id, preference))
                })
                .take(1_000)
                .collect();
        normalize_ids(&mut self.preview.pinned_tab_ids, 500);
        self.preview.focused_pane_id = bounded_optional(self.preview.focused_pane_id.take(), 256);
        self.preview.split_sizes =
            normalize_split_sizes(std::mem::take(&mut self.preview.split_sizes));
        self.preview.layout.normalize();
        let mut recovery = crate::EditorBufferRegistry::default();
        recovery.restore_recovery(std::mem::take(&mut self.preview.editor_recovery));
        self.preview.editor_recovery = recovery.recovery_snapshot();
        normalize_ids(&mut self.terminal.tab_order, 500);
        self.terminal.selected_terminal_id =
            bounded_optional(self.terminal.selected_terminal_id.take(), 256);
        normalize_ids(&mut self.right_rail.activity_order, 200);
        self.right_rail.selected_activity_id =
            bounded_optional(self.right_rail.selected_activity_id.take(), 256);
        normalize_ids(&mut self.composer.terminal_ids, 64);
        self.composer.runtime_selections_by_agent =
            std::mem::take(&mut self.composer.runtime_selections_by_agent)
                .into_iter()
                .filter_map(|(agent_id, mut selection)| {
                    if selection.agent_id != agent_id
                        || !normalize_runtime_selection(&mut selection)
                    {
                        return None;
                    }
                    Some((agent_id, selection))
                })
                .take(64)
                .collect();
        let legacy_runtime_selections = self
            .composer
            .runtime_selections_by_agent
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut runtime_selections_by_model = normalize_runtime_selection_preferences(
            std::mem::take(&mut self.composer.runtime_selections_by_model),
        );
        for selection in legacy_runtime_selections {
            if runtime_selections_by_model
                .iter()
                .any(|existing| runtime_selection_identity_matches(existing, &selection))
            {
                continue;
            }
            if runtime_selections_by_model.len() >= RUNTIME_SELECTION_PREFERENCE_LIMIT {
                break;
            }
            runtime_selections_by_model.push(selection);
        }
        self.composer.runtime_selections_by_model = runtime_selections_by_model;
        normalize_ids(&mut self.agent_tab_order, 256);
        self.terminal_tab_titles = std::mem::take(&mut self.terminal_tab_titles)
            .into_iter()
            .filter_map(|(key, value)| {
                let key = bounded_required(&key, 256)?;
                let value = bounded_required(&value, 160)?;
                Some((key, value))
            })
            .take(500)
            .collect();
        self.migration.source_schema = bounded_optional(self.migration.source_schema.take(), 160);
        Ok(())
    }

    pub fn cleanup_stale_ids(&mut self, references: &UiStateReferences) {
        // An empty set is a valid authoritative result when the database has
        // no records.  Treating it as "unknown" would retain stale ids after a
        // user removed the last workspace/session/terminal.
        self.sidebar
            .project_order
            .retain(|id| references.project_ids.contains(id));
        self.sidebar
            .collapsed_project_ids
            .retain(|id| references.project_ids.contains(id));
        self.sidebar
            .project_location_preferences
            .retain(|id, _| references.project_ids.contains(id));
        self.sidebar
            .collapsed_workspace_ids
            .retain(|id| references.workspace_ids.contains(id));
        self.sidebar
            .session_order
            .retain(|id| references.session_ids.contains(id));
        self.sidebar
            .pinned_session_ids
            .retain(|id| references.session_ids.contains(id));
        self.terminal
            .tab_order
            .retain(|id| references.terminal_ids.contains(id));
        self.composer
            .terminal_ids
            .retain(|id| references.terminal_ids.contains(id));
        self.terminal_tab_titles
            .retain(|id, _| references.terminal_ids.contains(id));
        self.preview.layout.tabs.retain(|_, tab| {
            !matches!(&tab.target, crate::PreviewTarget::Terminal { terminal_id } if !references.terminal_ids.contains(terminal_id))
        });
        self.preview.layout.normalize();
        self.preview
            .pinned_tab_ids
            .retain(|id| self.preview.layout.tabs.contains_key(id));
        if self
            .terminal
            .selected_terminal_id
            .as_ref()
            .is_some_and(|id| !references.terminal_ids.contains(id))
        {
            self.terminal.selected_terminal_id = None;
        }
        let selected_workspace_is_stale = self
            .workbench
            .selected_workspace_id
            .as_ref()
            .is_some_and(|id| !references.workspace_ids.contains(id));
        if selected_workspace_is_stale {
            self.workbench.selected_workspace_id = None;
            self.workbench.selected_session_id = None;
            self.workbench.selected_file_path = None;
            self.workbench.selected_git_path = None;
            self.terminal.selected_terminal_id = None;
            self.preview = PreviewUiState::default();
        }
        if self
            .workbench
            .selected_session_id
            .as_ref()
            .is_some_and(|id| !references.session_ids.contains(id))
        {
            self.workbench.selected_session_id = None;
        }
        self.right_rail
            .activity_order
            .retain(|id| references.plugin_ids.contains(id));
        if self
            .right_rail
            .selected_activity_id
            .as_ref()
            .is_some_and(|id| !references.plugin_ids.contains(id))
        {
            self.right_rail.selected_activity_id = None;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UiStateReferences {
    pub project_ids: BTreeSet<String>,
    pub workspace_ids: BTreeSet<String>,
    pub session_ids: BTreeSet<String>,
    pub terminal_ids: BTreeSet<String>,
    pub plugin_ids: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct UiStateStore {
    path: PathBuf,
    corrupt_backup_limit: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiStateLoad {
    pub state: DesktopUiStateV1,
    pub recovered_corrupt_state: bool,
    pub corrupt_backup_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiStateBackupMetadata {
    pub schema: String,
    pub source_file: String,
    pub backup_file: String,
    pub size_bytes: u64,
    pub checksum: String,
    pub source_schema: Option<String>,
}

impl UiStateStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            corrupt_backup_limit: DEFAULT_CORRUPT_BACKUP_LIMIT,
        }
    }

    pub fn with_corrupt_backup_limit(mut self, limit: usize) -> Self {
        self.corrupt_backup_limit = limit.max(1);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the current state without renaming or writing any files.  Desktop
    /// shells use this for their first frame before the runtime home lock is
    /// available; corruption quarantine is deferred to `load_or_default` after
    /// the lock has been acquired.
    pub fn load_read_only(&self) -> Result<UiStateLoad, UiStateError> {
        self.load_internal(None)
    }

    pub fn load_or_default(&self, now_ms: i64) -> Result<UiStateLoad, UiStateError> {
        self.load_internal(Some(now_ms))
    }

    fn load_internal(&self, quarantine_at_ms: Option<i64>) -> Result<UiStateLoad, UiStateError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(UiStateLoad {
                    state: DesktopUiStateV1::default(),
                    recovered_corrupt_state: false,
                    corrupt_backup_path: None,
                });
            }
            Err(error) => return Err(error.into()),
        };
        match decode_and_migrate(&bytes) {
            Ok(mut state) => {
                state.normalize()?;
                Ok(UiStateLoad {
                    state,
                    recovered_corrupt_state: false,
                    corrupt_backup_path: None,
                })
            }
            Err(UiStateError::UnsupportedVersion(version)) => {
                Err(UiStateError::UnsupportedVersion(version))
            }
            Err(_) => {
                let backup = quarantine_at_ms
                    .map(|now_ms| self.back_up_corrupt_state(now_ms))
                    .transpose()?;
                Ok(UiStateLoad {
                    state: DesktopUiStateV1::default(),
                    recovered_corrupt_state: true,
                    corrupt_backup_path: backup,
                })
            }
        }
    }

    pub fn save(&self, state: &DesktopUiStateV1) -> Result<(), UiStateError> {
        let mut normalized = state.clone();
        normalized.normalize()?;
        let bytes = serde_json::to_vec_pretty(&normalized)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp_path = self.temp_path();
        let result = (|| {
            let mut file = private_create(&temp_path)?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            replace_file(&temp_path, &self.path)?;
            sync_parent(&self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    /// Copy the versioned UI-state file into an explicit backup directory and
    /// return only bounded metadata.  The caller is responsible for pairing
    /// this artifact with the SQLite backup manifest.
    pub fn backup_snapshot(
        &self,
        backup_dir: impl AsRef<Path>,
        now_ms: i64,
    ) -> Result<UiStateBackupMetadata, UiStateError> {
        let backup_dir = backup_dir.as_ref();
        fs::create_dir_all(backup_dir)?;
        let source = self.path.clone();
        if !source.exists() {
            self.save(&DesktopUiStateV1::default())?;
        }
        let state = self.load_or_default(now_ms)?.state;
        let source_name = source
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("desktop-ui-state.json");
        let backup = backup_dir.join(format!("{source_name}.backup-{now_ms}.json"));
        fs::copy(&source, &backup)?;
        let bytes = fs::read(&backup)?;
        let checksum = sha256_bytes(&bytes);
        Ok(UiStateBackupMetadata {
            schema: "desktop-ui-state-backup.v2".to_string(),
            source_file: source_name.to_string(),
            backup_file: backup
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("desktop-ui-state.json")
                .to_string(),
            size_bytes: bytes.len() as u64,
            checksum,
            source_schema: state.migration.source_schema,
        })
    }

    fn temp_path(&self) -> PathBuf {
        let name = self
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("desktop-ui-state.json");
        self.path
            .with_file_name(format!(".{name}.tmp-{}", std::process::id()))
    }

    fn back_up_corrupt_state(&self, now_ms: i64) -> Result<PathBuf, UiStateError> {
        let name = self
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("desktop-ui-state.json");
        let backup = self
            .path
            .with_file_name(format!("{name}.corrupt-{now_ms}.json"));
        fs::rename(&self.path, &backup)?;
        self.prune_corrupt_backups()?;
        Ok(backup)
    }

    fn prune_corrupt_backups(&self) -> Result<(), UiStateError> {
        let Some(parent) = self.path.parent() else {
            return Ok(());
        };
        let prefix = format!(
            "{}.corrupt-",
            self.path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("desktop-ui-state.json")
        );
        let mut backups = fs::read_dir(parent)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".json"))
            })
            .collect::<Vec<_>>();
        backups.sort();
        let remove_count = backups.len().saturating_sub(self.corrupt_backup_limit);
        for path in backups.into_iter().take(remove_count) {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Clone)]
pub struct ThrottledUiStateWriter {
    store: UiStateStore,
    delay_ms: u64,
    pending: Option<DesktopUiStateV1>,
    flush_at_ms: Option<i64>,
}

impl ThrottledUiStateWriter {
    pub fn new(store: UiStateStore, delay_ms: u64) -> Self {
        Self {
            store,
            delay_ms: delay_ms.clamp(MIN_UI_STATE_WRITE_DELAY_MS, MAX_UI_STATE_WRITE_DELAY_MS),
            pending: None,
            flush_at_ms: None,
        }
    }

    pub fn queue(&mut self, state: DesktopUiStateV1, now_ms: i64) {
        self.pending = Some(state);
        self.flush_at_ms
            .get_or_insert_with(|| now_ms.saturating_add(self.delay_ms as i64));
    }

    pub fn flush_if_due(&mut self, now_ms: i64) -> Result<bool, UiStateError> {
        if self.flush_at_ms.is_none_or(|deadline| now_ms < deadline) {
            return Ok(false);
        }
        self.flush()?;
        Ok(true)
    }

    pub fn flush(&mut self) -> Result<(), UiStateError> {
        let Some(state) = self.pending.as_ref() else {
            self.flush_at_ms = None;
            return Ok(());
        };
        self.store.save(state)?;
        self.pending = None;
        self.flush_at_ms = None;
        Ok(())
    }

    pub fn has_pending_write(&self) -> bool {
        self.pending.is_some()
    }
}

fn decode_and_migrate(bytes: &[u8]) -> Result<DesktopUiStateV1, UiStateError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    match version {
        0 => migrate_v0(value),
        1 => Ok(serde_json::from_value(value)?),
        other => Err(UiStateError::UnsupportedVersion(other)),
    }
}

fn migrate_v0(value: serde_json::Value) -> Result<DesktopUiStateV1, UiStateError> {
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct V0 {
        theme: Option<ThemeMode>,
        locale: Option<LocaleMode>,
        active_tab: Option<String>,
        sidebar_width: Option<f32>,
        right_rail_width: Option<f32>,
    }
    let old: V0 = serde_json::from_value(value)?;
    let mut state = DesktopUiStateV1::default();
    state.appearance.theme = old.theme.unwrap_or_default();
    state.appearance.locale = old.locale.unwrap_or_default();
    if let Some(active_tab) = old.active_tab {
        state.workbench.active_tab = active_tab;
    }
    if let Some(width) = old.sidebar_width {
        state.workbench.sidebar_width = width;
    }
    if let Some(width) = old.right_rail_width {
        state.workbench.right_rail_width = width;
    }
    state.migration.source_schema = Some("desktop-ui-state.v0".to_string());
    Ok(state)
}

fn bounded_required(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.chars().take(max_chars).collect())
}

fn bounded_optional(value: Option<String>, max_chars: usize) -> Option<String> {
    value.and_then(|value| bounded_required(&value, max_chars))
}

fn bounded_f32(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

fn normalize_ids(ids: &mut Vec<String>, limit: usize) {
    let mut seen = BTreeSet::new();
    *ids = std::mem::take(ids)
        .into_iter()
        .filter_map(|id| bounded_required(&id, 256))
        .filter(|id| seen.insert(id.clone()))
        .take(limit)
        .collect();
}

fn normalize_set(ids: &mut BTreeSet<String>, limit: usize) {
    let mut values = ids.iter().cloned().collect::<Vec<_>>();
    normalize_ids(&mut values, limit);
    *ids = values.into_iter().collect();
}

fn normalize_runtime_selection(selection: &mut SessionRuntimeSelection) -> bool {
    let Some(model_id) = bounded_runtime_value(&selection.model_id, 512) else {
        return false;
    };
    selection.model_id = model_id;
    selection.reasoning_effort = match selection.reasoning_effort.take() {
        Some(value) => {
            let Some(value) = bounded_runtime_value(&value, 128) else {
                return false;
            };
            Some(value)
        }
        None => None,
    };
    selection.mode_id = match selection.mode_id.take() {
        Some(value) => {
            let Some(value) = bounded_runtime_value(&value, 128) else {
                return false;
            };
            Some(value)
        }
        None => None,
    };
    if selection.config_values.len() > 64 {
        return false;
    }
    let Some(config_values) = std::mem::take(&mut selection.config_values)
        .into_iter()
        .map(|(key, value)| {
            Some((
                bounded_runtime_value(&key, 160)?,
                bounded_runtime_value(&value, 256)?,
            ))
        })
        .collect::<Option<BTreeMap<_, _>>>()
    else {
        return false;
    };
    selection.config_values = config_values;
    true
}

fn normalize_runtime_selection_preferences(
    selections: Vec<SessionRuntimeSelection>,
) -> Vec<SessionRuntimeSelection> {
    let mut normalized = Vec::<SessionRuntimeSelection>::new();
    for mut selection in selections {
        if !normalize_runtime_selection(&mut selection) {
            continue;
        }
        if let Some(existing) = normalized
            .iter_mut()
            .find(|existing| runtime_selection_identity_matches(existing, &selection))
        {
            *existing = selection;
        } else if normalized.len() < RUNTIME_SELECTION_PREFERENCE_LIMIT {
            normalized.push(selection);
        }
    }
    normalized
}

fn runtime_selection_identity_matches(
    left: &SessionRuntimeSelection,
    right: &SessionRuntimeSelection,
) -> bool {
    left.agent_id == right.agent_id
        && left.provider_profile_id == right.provider_profile_id
        && left.model_id == right.model_id
}

fn bounded_runtime_value(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.chars().count() <= max_chars).then(|| value.to_string())
}

fn normalize_split_sizes(sizes: Vec<f32>) -> Vec<f32> {
    let mut sizes = sizes
        .into_iter()
        .filter(|size| size.is_finite() && *size > 0.0)
        .take(16)
        .collect::<Vec<_>>();
    if sizes.is_empty() {
        return vec![1.0];
    }
    let total = sizes.iter().sum::<f32>();
    for size in &mut sizes {
        *size /= total;
    }
    sizes
}

fn private_create(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn workbench_defaults_keep_optional_right_panels_closed() {
        let state = WorkbenchUiState::default();
        assert!(!state.preview_visible);
        assert!(!state.right_rail_visible);
        assert_eq!(state.sidebar_width, 320.0);
        assert_eq!(state.preview_width, 520.0);
        assert_eq!(state.right_rail_width, 336.0);
    }

    #[test]
    fn state_normalizes_fonts_layout_ids_and_split_sizes() {
        let mut state = DesktopUiStateV1::default();
        state.appearance.interface_font.size = 99;
        state.appearance.code_font.weight = 1;
        state.workbench.sidebar_width = f32::NAN;
        state.sidebar.session_order = vec![" session_a ".into(), "session_a".into()];
        state.preview.split_sizes = vec![2.0, 2.0];
        state.normalize().unwrap();
        assert_eq!(state.appearance.interface_font.size, 24);
        assert_eq!(state.appearance.code_font.weight, 100);
        assert_eq!(state.workbench.sidebar_width, 320.0);
        assert_eq!(state.sidebar.session_order, vec!["session_a"]);
        assert_eq!(state.preview.split_sizes, vec![0.5, 0.5]);
    }

    #[test]
    fn sidebar_worktree_preferences_are_bounded_and_backward_compatible() {
        let mut state = DesktopUiStateV1::default();
        state.sidebar.hierarchy_mode = SidebarHierarchyMode::Detailed;
        state
            .sidebar
            .project_location_preferences
            .insert(" project-1 ".into(), NewSessionLocation::NewWorktree);
        state.normalize().unwrap();
        assert_eq!(state.sidebar.hierarchy_mode, SidebarHierarchyMode::Detailed);
        assert_eq!(
            state.sidebar.project_location_preferences.get("project-1"),
            Some(&NewSessionLocation::NewWorktree)
        );

        let mut value = serde_json::to_value(DesktopUiStateV1::default()).unwrap();
        let sidebar = value
            .get_mut("sidebar")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap();
        sidebar.remove("hierarchyMode");
        sidebar.remove("projectLocationPreferences");
        sidebar.remove("collapsedWorkspaceIds");
        let decoded = decode_and_migrate(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            decoded.sidebar.hierarchy_mode,
            SidebarHierarchyMode::Compact
        );
        assert!(decoded.sidebar.project_location_preferences.is_empty());
        assert!(decoded.sidebar.collapsed_workspace_ids.is_empty());
    }

    #[test]
    fn session_display_preferences_are_backward_compatible() {
        let mut value = serde_json::to_value(DesktopUiStateV1::default()).unwrap();
        let session = value
            .get_mut("session")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap();
        session.remove("enhancedCommandExecutionDisplay");

        let decoded = decode_and_migrate(&serde_json::to_vec(&value).unwrap()).unwrap();

        assert!(decoded.session.enhanced_command_execution_display);
    }

    #[test]
    fn state_normalizes_bounded_runtime_selections_by_agent() {
        let codex = AgentId::parse("codex").unwrap();
        let claude = AgentId::parse("claude").unwrap();
        let mut state = DesktopUiStateV1::default();
        state.composer.runtime_selections_by_agent.insert(
            codex.clone(),
            SessionRuntimeSelection {
                agent_id: codex.clone(),
                provider_profile_id: vibex_core::ProviderProfileId::new(),
                model_id: "  gpt-5  ".into(),
                reasoning_effort: Some(" high ".into()),
                mode_id: Some(" agent ".into()),
                config_values: BTreeMap::from([(" web_search ".into(), " true ".into())]),
            },
        );
        state.composer.runtime_selections_by_agent.insert(
            claude,
            SessionRuntimeSelection {
                agent_id: AgentId::parse("codex").unwrap(),
                provider_profile_id: vibex_core::ProviderProfileId::new(),
                model_id: "stale".into(),
                reasoning_effort: None,
                mode_id: None,
                config_values: BTreeMap::new(),
            },
        );
        state.composer.runtime_selections_by_agent.insert(
            AgentId::parse("opencode").unwrap(),
            SessionRuntimeSelection {
                agent_id: AgentId::parse("opencode").unwrap(),
                provider_profile_id: vibex_core::ProviderProfileId::new(),
                model_id: "x".repeat(513),
                reasoning_effort: None,
                mode_id: None,
                config_values: BTreeMap::new(),
            },
        );

        state.normalize().unwrap();

        let selection = state
            .composer
            .runtime_selections_by_agent
            .get(&codex)
            .unwrap();
        assert_eq!(selection.model_id, "gpt-5");
        assert_eq!(selection.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(selection.mode_id.as_deref(), Some("agent"));
        assert_eq!(
            selection.config_values,
            BTreeMap::from([("web_search".into(), "true".into())])
        );
        assert_eq!(state.composer.runtime_selections_by_agent.len(), 1);
        assert_eq!(
            state.composer.runtime_selections_by_model,
            vec![selection.clone()]
        );
    }

    #[test]
    fn runtime_selection_preferences_round_trip_in_ui_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop-ui-state.json");
        let store = UiStateStore::new(&path);
        let codex = AgentId::parse("codex").unwrap();
        let mut state = DesktopUiStateV1::default();
        let selection = SessionRuntimeSelection {
            agent_id: codex.clone(),
            provider_profile_id: vibex_core::ProviderProfileId::new(),
            model_id: "gpt-5".into(),
            reasoning_effort: Some("medium".into()),
            mode_id: Some("agent".into()),
            config_values: BTreeMap::from([("web_search".into(), "true".into())]),
        };
        state
            .composer
            .runtime_selections_by_agent
            .insert(codex.clone(), selection.clone());
        let mut alternate = selection.clone();
        alternate.model_id = "gpt-5-mini".into();
        alternate.reasoning_effort = Some("low".into());
        state
            .composer
            .runtime_selections_by_model
            .extend([selection.clone(), alternate.clone()]);

        store.save(&state).unwrap();
        let loaded = store.load_or_default(1).unwrap();

        assert_eq!(
            loaded
                .state
                .composer
                .runtime_selections_by_agent
                .get(&codex),
            Some(&selection)
        );
        assert_eq!(
            loaded.state.composer.runtime_selections_by_model,
            vec![selection, alternate]
        );
    }

    #[test]
    fn current_v1_without_runtime_preferences_defaults_to_empty_collections() {
        let mut value = serde_json::to_value(DesktopUiStateV1::default()).unwrap();
        let composer = value
            .get_mut("composer")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap();
        composer.remove("runtimeSelectionsByAgent");
        composer.remove("runtimeSelectionsByModel");

        let decoded = decode_and_migrate(&serde_json::to_vec(&value).unwrap()).unwrap();

        assert!(decoded.composer.runtime_selections_by_agent.is_empty());
        assert!(decoded.composer.runtime_selections_by_model.is_empty());
    }

    #[test]
    fn runtime_preferences_keep_model_values_over_legacy_seeds_and_latest_duplicates() {
        let agent_id = AgentId::parse("codex").unwrap();
        let provider_profile_id = vibex_core::ProviderProfileId::new();
        let selection = |model: &str, effort: &str| SessionRuntimeSelection {
            agent_id: agent_id.clone(),
            provider_profile_id: provider_profile_id.clone(),
            model_id: model.into(),
            reasoning_effort: Some(effort.into()),
            mode_id: None,
            config_values: BTreeMap::new(),
        };
        let mut state = DesktopUiStateV1::default();
        state
            .composer
            .runtime_selections_by_agent
            .insert(agent_id.clone(), selection("gpt-5", "low"));
        state.composer.runtime_selections_by_model = vec![
            selection("gpt-5", "low"),
            selection("gpt-5-mini", "medium"),
            selection("gpt-5", "high"),
        ];

        state.normalize().unwrap();

        assert_eq!(state.composer.runtime_selections_by_model.len(), 2);
        assert_eq!(
            state.composer.runtime_selections_by_model[0]
                .reasoning_effort
                .as_deref(),
            Some("high")
        );
        assert_eq!(
            state.composer.runtime_selections_by_model[1].model_id,
            "gpt-5-mini"
        );
    }

    #[test]
    fn read_only_load_reports_corruption_without_mutating_the_home() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop-ui-state.json");
        fs::write(&path, b"not json").unwrap();
        let store = UiStateStore::new(&path);

        let loaded = store.load_read_only().unwrap();

        assert!(loaded.recovered_corrupt_state);
        assert!(loaded.corrupt_backup_path.is_none());
        assert_eq!(fs::read(&path).unwrap(), b"not json");
        assert_eq!(loaded.state, DesktopUiStateV1::default());
    }

    #[test]
    fn corrupt_state_is_quarantined_and_defaults_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop-ui-state.json");
        fs::write(&path, b"not json").unwrap();
        let store = UiStateStore::new(&path);
        let loaded = store.load_or_default(123).unwrap();
        assert!(loaded.recovered_corrupt_state);
        assert!(!path.exists());
        assert!(loaded.corrupt_backup_path.unwrap().exists());
        assert_eq!(loaded.state, DesktopUiStateV1::default());
    }

    #[test]
    fn atomic_store_and_throttled_exit_flush_round_trip_without_sensitive_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop-ui-state.json");
        let store = UiStateStore::new(&path);
        let mut writer = ThrottledUiStateWriter::new(store.clone(), 1);
        let mut state = DesktopUiStateV1::default();
        state.workbench.selected_session_id = Some("session_fixture".into());
        writer.queue(state.clone(), 1_000);
        assert!(!writer.flush_if_due(1_099).unwrap());
        writer.flush().unwrap();

        let serialized = fs::read_to_string(&path).unwrap();
        for forbidden in [
            "prompt",
            "messageContent",
            "terminalOutput",
            "gitPatch",
            "fileContent",
            "secret",
            "cookie",
            "privateKey",
            "authToken",
            "url",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "leaked forbidden field {forbidden}"
            );
        }
        let loaded = store.load_or_default(2_000).unwrap();
        assert_eq!(
            loaded.state.workbench.selected_session_id,
            state.workbench.selected_session_id
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn v0_migration_is_deterministic_and_unknown_future_version_is_not_destroyed() {
        let migrated =
            decode_and_migrate(br#"{"theme":"dark","activeTab":"files","sidebarWidth":320}"#)
                .unwrap();
        assert_eq!(migrated.appearance.theme, ThemeMode::Dark);
        assert_eq!(migrated.workbench.active_tab, "files");
        assert_eq!(
            migrated.migration.source_schema.as_deref(),
            Some("desktop-ui-state.v0")
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop-ui-state.json");
        fs::write(&path, br#"{"schemaVersion":99}"#).unwrap();
        let store = UiStateStore::new(&path);
        let error = store.load_or_default(1).unwrap_err();
        assert!(matches!(error, UiStateError::UnsupportedVersion(99)));
        assert!(path.exists());
    }

    #[test]
    fn authoritative_empty_references_remove_all_stale_ids() {
        let mut state = DesktopUiStateV1::default();
        state.sidebar.project_order = vec!["project".to_string()];
        state.sidebar.session_order = vec!["session".to_string()];
        state.terminal.tab_order = vec!["terminal".to_string()];
        state.composer.terminal_ids = vec!["terminal".to_string()];
        state
            .terminal_tab_titles
            .insert("terminal".to_string(), "T".to_string());
        state.workbench.selected_workspace_id = Some("workspace".to_string());
        state.workbench.selected_session_id = Some("session".to_string());
        state.workbench.selected_file_path = Some("src/main.rs".to_string());
        state.workbench.selected_git_path = Some("src/lib.rs".to_string());
        state.terminal.selected_terminal_id = Some("terminal".to_string());
        state.right_rail.activity_order = vec!["plugin".to_string()];
        state.preview.layout.open(
            crate::PreviewTarget::Terminal {
                terminal_id: "terminal".to_string(),
            },
            None,
            1,
        );

        state.cleanup_stale_ids(&UiStateReferences::default());
        assert!(state.sidebar.project_order.is_empty());
        assert!(state.sidebar.session_order.is_empty());
        assert!(state.terminal.tab_order.is_empty());
        assert!(state.composer.terminal_ids.is_empty());
        assert!(state.terminal_tab_titles.is_empty());
        assert!(state.workbench.selected_workspace_id.is_none());
        assert!(state.workbench.selected_session_id.is_none());
        assert!(state.workbench.selected_file_path.is_none());
        assert!(state.workbench.selected_git_path.is_none());
        assert!(state.terminal.selected_terminal_id.is_none());
        assert!(state.right_rail.activity_order.is_empty());
        assert!(state.preview.layout.tabs.is_empty());
    }

    #[test]
    fn ui_state_backup_returns_redacted_metadata_and_keeps_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop-ui-state.json");
        let store = UiStateStore::new(&path);
        let mut state = DesktopUiStateV1::default();
        state.migration.source_schema = Some("desktop-ui-state.v0".to_string());
        state.migration.migrated_at_ms = Some(123);
        store.save(&state).unwrap();
        let metadata = store
            .backup_snapshot(dir.path().join("backup"), 123)
            .unwrap();
        assert_eq!(metadata.schema, "desktop-ui-state-backup.v2");
        assert_eq!(
            metadata.source_schema.as_deref(),
            Some("desktop-ui-state.v0")
        );
        assert!(metadata.size_bytes > 0);
        assert_eq!(metadata.checksum.len(), 64);
        assert!(path.exists());
        let serialized = serde_json::to_string(&metadata).unwrap();
        assert!(!serialized.contains("prompt"));
        assert!(!serialized.contains("secret"));
    }
}

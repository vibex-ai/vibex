use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value as JsonValue, json};
use vibex_core::{
    ProviderBindingMetadata, ProviderKind, ProviderNativeConfigFileKind,
    ProviderNativeExportApplyRequest, ProviderNativeExportApplyResult,
    ProviderNativeExportApplyStatus, ProviderNativeExportFilePlan, ProviderNativeExportFileStatus,
    ProviderNativeExportListRequest, ProviderNativeExportMode, ProviderNativeExportOperationKind,
    ProviderNativeExportPreview, ProviderNativeExportPreviewRequest,
    ProviderNativeExportRecordSummary, ProviderNativeExportRollbackRequest,
    ProviderNativeExportRollbackResult, ProviderNativeExportRollbackStatus,
    ProviderNativeExportSource, ProviderProfile, RequestId, VibexError, VibexResult,
    unix_timestamp_ms,
};
use vibex_db::{ProviderNativeExportRepository, ProviderProfileRepository};

use crate::ProviderConfigService;

const CODEX_MARKER_START: &str = "# >>> VIBEX MANAGED PROVIDER EXPORT";
const CODEX_MARKER_END: &str = "# <<< VIBEX MANAGED PROVIDER EXPORT";

#[derive(Debug)]
struct ApplyFileError {
    error: Box<VibexError>,
    restored: bool,
}

#[derive(Debug, Clone, Default)]
struct NativeExportRoots {
    codex_root: Option<PathBuf>,
    claude_root: Option<PathBuf>,
}

impl ProviderConfigService {
    pub fn preview_native_export(
        &self,
        request: ProviderNativeExportPreviewRequest,
    ) -> VibexResult<ProviderNativeExportPreview> {
        let conn = self.open_connection()?;
        let profile = ProviderProfileRepository::get(&conn, &request.provider_profile_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_native_export_profile_not_found",
                    "provider profile was not found for native export",
                )
                .with_diagnostic("providerProfileId", request.provider_profile_id.as_str())
            })?;
        let preview =
            preview_native_export_with_roots(&profile, request.clone(), Default::default())?;
        if request.persist {
            ProviderNativeExportRepository::insert_preview(&conn, &preview)?;
        }
        Ok(preview)
    }

    pub fn apply_native_export(
        &self,
        request: ProviderNativeExportApplyRequest,
    ) -> VibexResult<ProviderNativeExportApplyResult> {
        let conn = self.open_connection()?;
        let preview = ProviderNativeExportRepository::get_preview(&conn, &request.export_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_native_export_preview_not_found",
                    "native export preview was not found",
                )
                .with_diagnostic("exportId", request.export_id.as_str())
            })?;
        let result = apply_preview(preview);
        ProviderNativeExportRepository::record_apply_result(&conn, &result)?;
        Ok(result)
    }

    pub fn rollback_native_export(
        &self,
        request: ProviderNativeExportRollbackRequest,
    ) -> VibexResult<ProviderNativeExportRollbackResult> {
        let conn = self.open_connection()?;
        let preview = match ProviderNativeExportRepository::get_preview(&conn, &request.export_id)?
        {
            Some(preview) => preview,
            None => {
                return Ok(ProviderNativeExportRollbackResult {
                    export_id: request.export_id,
                    status: ProviderNativeExportRollbackStatus::NotFound,
                    files: Vec::new(),
                    diagnostics: vec![metadata(
                        "provider_native_export_rollback_not_found",
                        "native export record was not found",
                    )],
                    rolled_back_at_ms: unix_timestamp_ms(),
                });
            }
        };
        let result = rollback_preview(preview);
        ProviderNativeExportRepository::record_rollback_result(&conn, &result)?;
        Ok(result)
    }

    pub fn list_native_exports(
        &self,
        request: ProviderNativeExportListRequest,
    ) -> VibexResult<Vec<ProviderNativeExportRecordSummary>> {
        let conn = self.open_connection()?;
        ProviderNativeExportRepository::list(&conn, request)
    }
}

fn preview_native_export_with_roots(
    profile: &ProviderProfile,
    request: ProviderNativeExportPreviewRequest,
    roots: NativeExportRoots,
) -> VibexResult<ProviderNativeExportPreview> {
    let export_id = RequestId::new();
    let mut diagnostics = Vec::new();
    let files = match request.mode {
        ProviderNativeExportMode::ProviderProfile => match request.source {
            ProviderNativeExportSource::Codex => {
                vec![codex_profile_plan(&export_id, profile, roots.codex_root)?]
            }
            ProviderNativeExportSource::Claude => {
                vec![claude_profile_plan(&export_id, profile, roots.claude_root)?]
            }
        },
        ProviderNativeExportMode::Mcp
        | ProviderNativeExportMode::Skills
        | ProviderNativeExportMode::Prompts
        | ProviderNativeExportMode::Combined => {
            diagnostics.push(metadata(
                "provider_native_export_blocked",
                "native export for this resource mode is not enabled yet; session injection remains the default",
            ));
            vec![blocked_plan(
                &export_id,
                request.source,
                target_file_kind(request.source),
                target_path(request.source, roots).join(target_file_name(request.source)),
                "unsupported native export mode",
            )]
        }
    };

    Ok(ProviderNativeExportPreview {
        export_id,
        provider_profile_id: profile.id.clone(),
        source: request.source,
        mode: request.mode,
        files,
        diagnostics,
        created_at_ms: unix_timestamp_ms(),
    })
}

fn codex_profile_plan(
    export_id: &RequestId,
    profile: &ProviderProfile,
    root_override: Option<PathBuf>,
) -> VibexResult<ProviderNativeExportFilePlan> {
    if profile.kind != ProviderKind::Codex {
        return Ok(blocked_plan(
            export_id,
            ProviderNativeExportSource::Codex,
            ProviderNativeConfigFileKind::CodexConfigToml,
            codex_config_root(root_override).join("config.toml"),
            "selected profile is not a Codex profile",
        ));
    }

    let target = codex_config_root(root_override).join("config.toml");
    let before = read_optional(&target)?;
    let block = codex_managed_block(profile);
    let after = match before.as_deref() {
        None => block.clone(),
        Some(current)
            if current.contains(CODEX_MARKER_START) && current.contains(CODEX_MARKER_END) =>
        {
            replace_marked_block(current, &block).unwrap_or_else(|| current.to_string())
        }
        Some(current) if current.trim().is_empty() => block.clone(),
        Some(current) => {
            return Ok(blocked_plan(
                export_id,
                ProviderNativeExportSource::Codex,
                ProviderNativeConfigFileKind::CodexConfigToml,
                target,
                format!(
                    "existing Codex config has no Vibex marker; preserving {} bytes of user-managed TOML",
                    current.len()
                ),
            ));
        }
    };
    Ok(ready_plan(
        export_id,
        ProviderNativeExportSource::Codex,
        ProviderNativeConfigFileKind::CodexConfigToml,
        target,
        before.unwrap_or_default(),
        after,
        Some("Vibex managed TOML block".to_string()),
    ))
}

fn claude_profile_plan(
    export_id: &RequestId,
    profile: &ProviderProfile,
    root_override: Option<PathBuf>,
) -> VibexResult<ProviderNativeExportFilePlan> {
    if profile.kind != ProviderKind::Claude {
        return Ok(blocked_plan(
            export_id,
            ProviderNativeExportSource::Claude,
            ProviderNativeConfigFileKind::ClaudeSettingsJson,
            claude_config_root(root_override).join("settings.json"),
            "selected profile is not a Claude profile",
        ));
    }

    let target = claude_config_root(root_override).join("settings.json");
    let before = read_optional(&target)?;
    let mut value = match before.as_deref() {
        Some(current) if !current.trim().is_empty() => serde_json::from_str::<JsonValue>(current)
            .map_err(|err| {
            VibexError::validation(
                "provider_native_export_unsafe_target",
                "Claude settings.json is not valid JSON",
            )
            .with_diagnostic("targetPath", target.display().to_string())
            .with_diagnostic("error", err.to_string())
        })?,
        _ => json!({}),
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(blocked_plan(
            export_id,
            ProviderNativeExportSource::Claude,
            ProviderNativeConfigFileKind::ClaudeSettingsJson,
            target,
            "Claude settings.json root is not an object",
        ));
    };
    object.insert(
        "vibex".to_string(),
        json!({
            "managedBy": "vibex",
            "providerProfileId": profile.id.as_str(),
            "displayName": profile.display_name,
            "baseUrl": profile.base_url,
            "defaultModel": profile.default_model,
            "reasoningEffort": profile.reasoning_effort,
            "secretPolicy": "use environment or Vibex secret references; plaintext secrets are not exported",
        }),
    );
    let after = serde_json::to_string_pretty(&value).map_err(|err| {
        VibexError::storage(
            "provider_native_export_record_failed",
            "failed to encode Claude native export preview",
        )
        .with_diagnostic("error", err.to_string())
    })? + "\n";
    Ok(ready_plan(
        export_id,
        ProviderNativeExportSource::Claude,
        ProviderNativeConfigFileKind::ClaudeSettingsJson,
        target,
        before.unwrap_or_default(),
        after,
        Some("Top-level settings.json field: vibex".to_string()),
    ))
}

fn apply_preview(preview: ProviderNativeExportPreview) -> ProviderNativeExportApplyResult {
    let applied_at_ms = unix_timestamp_ms();
    let mut diagnostics = Vec::new();
    let mut files = Vec::new();

    for mut file in preview.files {
        if file.status == ProviderNativeExportFileStatus::Blocked {
            files.push(file);
            continue;
        }
        if file.operation_kind == ProviderNativeExportOperationKind::NoOp {
            file.status = ProviderNativeExportFileStatus::NoOp;
            files.push(file);
            continue;
        }

        match apply_file_plan(&file) {
            Ok(()) => file.status = ProviderNativeExportFileStatus::Applied,
            Err(err) => {
                let diagnostic =
                    metadata("provider_native_export_apply_failed", err.error.to_string());
                file.diagnostics.push(diagnostic.clone());
                diagnostics.push(diagnostic);
                file.status = if err.restored {
                    ProviderNativeExportFileStatus::Restored
                } else {
                    ProviderNativeExportFileStatus::Failed
                };
            }
        }
        files.push(file);
    }

    let ready_count = files
        .iter()
        .filter(|file| file.status == ProviderNativeExportFileStatus::Applied)
        .count();
    let failed_count = files
        .iter()
        .filter(|file| file.status == ProviderNativeExportFileStatus::Failed)
        .count();
    let restored_after_failure_count = files
        .iter()
        .filter(|file| file.status == ProviderNativeExportFileStatus::Restored)
        .count();
    let status = if failed_count == 0 {
        if restored_after_failure_count > 0 {
            ProviderNativeExportApplyStatus::FailedRestored
        } else {
            ProviderNativeExportApplyStatus::Applied
        }
    } else if ready_count > 0 {
        ProviderNativeExportApplyStatus::PartiallyApplied
    } else if restored_after_failure_count > 0 {
        ProviderNativeExportApplyStatus::FailedRestored
    } else {
        ProviderNativeExportApplyStatus::FailedUnrestored
    };

    ProviderNativeExportApplyResult {
        export_id: preview.export_id,
        status,
        files,
        diagnostics,
        applied_at_ms,
    }
}

fn rollback_preview(preview: ProviderNativeExportPreview) -> ProviderNativeExportRollbackResult {
    let rolled_back_at_ms = unix_timestamp_ms();
    let mut diagnostics = Vec::new();
    let mut files = Vec::new();

    for mut file in preview.files {
        match restore_from_backup(&file) {
            Ok(()) => file.status = ProviderNativeExportFileStatus::Restored,
            Err(err) => {
                let diagnostic =
                    metadata("provider_native_export_rollback_failed", err.to_string());
                file.diagnostics.push(diagnostic.clone());
                diagnostics.push(diagnostic);
                file.status = ProviderNativeExportFileStatus::Failed;
            }
        }
        files.push(file);
    }

    let restored_count = files
        .iter()
        .filter(|file| file.status == ProviderNativeExportFileStatus::Restored)
        .count();
    let failed_count = files
        .iter()
        .filter(|file| file.status == ProviderNativeExportFileStatus::Failed)
        .count();
    let status = if failed_count == 0 && restored_count > 0 {
        ProviderNativeExportRollbackStatus::Restored
    } else if restored_count > 0 {
        ProviderNativeExportRollbackStatus::PartiallyRestored
    } else {
        ProviderNativeExportRollbackStatus::Failed
    };

    ProviderNativeExportRollbackResult {
        export_id: preview.export_id,
        status,
        files,
        diagnostics,
        rolled_back_at_ms,
    }
}

fn apply_file_plan(file: &ProviderNativeExportFilePlan) -> Result<(), ApplyFileError> {
    let target = PathBuf::from(&file.target_path);
    let before = read_optional(&target).map_err(unrestored)?;
    if file.source == ProviderNativeExportSource::Codex
        && before.as_deref().is_some_and(|current| {
            !current.trim().is_empty() && !current.contains(CODEX_MARKER_START)
        })
    {
        return Err(unrestored(
            VibexError::validation(
                "provider_native_export_unsafe_target",
                "Codex config changed to an unmarked user-managed file after preview",
            )
            .with_diagnostic("targetPath", file.target_path.clone()),
        ));
    }

    let parent = target
        .parent()
        .ok_or_else(|| {
            VibexError::validation(
                "provider_native_export_unsafe_target",
                "native export target has no parent directory",
            )
            .with_diagnostic("targetPath", file.target_path.clone())
        })
        .map_err(unrestored)?;
    fs::create_dir_all(parent)
        .map_err(|err| {
            VibexError::storage(
                "provider_native_export_backup_failed",
                "failed to create native export parent directory",
            )
            .with_diagnostic("targetPath", file.target_path.clone())
            .with_diagnostic("error", err.to_string())
        })
        .map_err(unrestored)?;

    if target.exists() {
        let backup = file
            .backup_path
            .as_ref()
            .ok_or_else(|| {
                VibexError::storage(
                    "provider_native_export_backup_failed",
                    "native export backup path was missing",
                )
            })
            .map_err(unrestored)?;
        fs::copy(&target, backup)
            .map_err(|err| {
                VibexError::storage(
                    "provider_native_export_backup_failed",
                    "failed to back up native config",
                )
                .with_diagnostic("targetPath", file.target_path.clone())
                .with_diagnostic("backupPath", backup.clone())
                .with_diagnostic("error", err.to_string())
            })
            .map_err(unrestored)?;
    }

    let temp = file
        .temp_path
        .as_ref()
        .ok_or_else(|| {
            VibexError::storage(
                "provider_native_export_temp_write_failed",
                "native export temp path was missing",
            )
        })
        .map_err(unrestored)?;
    if let Err(err) = fs::write(temp, &file.redacted_after) {
        let restored = restore_from_backup(file).is_ok();
        return Err(ApplyFileError {
            error: Box::new(
                VibexError::storage(
                    "provider_native_export_temp_write_failed",
                    "failed to write native export temp file",
                )
                .with_diagnostic("tempPath", temp.clone())
                .with_diagnostic("error", err.to_string()),
            ),
            restored,
        });
    }
    if let Err(err) = fs::rename(temp, &target) {
        let restored = restore_from_backup(file).is_ok();
        return Err(ApplyFileError {
            error: Box::new(
                VibexError::storage(
                    "provider_native_export_atomic_replace_failed",
                    "failed to atomically replace native config",
                )
                .with_diagnostic("targetPath", file.target_path.clone())
                .with_diagnostic("error", err.to_string()),
            ),
            restored,
        });
    }
    Ok(())
}

fn unrestored(error: VibexError) -> ApplyFileError {
    ApplyFileError {
        error: Box::new(error),
        restored: false,
    }
}

fn restore_from_backup(file: &ProviderNativeExportFilePlan) -> VibexResult<()> {
    let backup = file.backup_path.as_ref().ok_or_else(|| {
        VibexError::validation(
            "provider_native_export_rollback_failed",
            "native export file has no Vibex-created backup to restore",
        )
        .with_diagnostic("operationId", file.operation_id.as_str())
    })?;
    let backup_path = PathBuf::from(backup);
    if !backup_path.exists() {
        return Err(VibexError::validation(
            "provider_native_export_rollback_failed",
            "Vibex-created backup file is missing",
        )
        .with_diagnostic("backupPath", backup.clone()));
    }
    fs::copy(&backup_path, &file.target_path).map_err(|err| {
        VibexError::storage(
            "provider_native_export_restore_failed",
            "failed to restore native config from Vibex backup",
        )
        .with_diagnostic("targetPath", file.target_path.clone())
        .with_diagnostic("backupPath", backup.clone())
        .with_diagnostic("error", err.to_string())
    })?;
    Ok(())
}

fn ready_plan(
    export_id: &RequestId,
    source: ProviderNativeExportSource,
    file_kind: ProviderNativeConfigFileKind,
    target: PathBuf,
    before: String,
    after: String,
    marker: Option<String>,
) -> ProviderNativeExportFilePlan {
    let operation_id = RequestId::new();
    let operation_kind = if before == after {
        ProviderNativeExportOperationKind::NoOp
    } else if before.is_empty() && !target.exists() {
        ProviderNativeExportOperationKind::CreateFile
    } else {
        ProviderNativeExportOperationKind::UpdateFile
    };
    let backup_path = if target.exists() {
        Some(sibling_path(&target, export_id, "bak"))
    } else {
        None
    };
    let temp_path = Some(sibling_path(&target, export_id, "tmp"));
    let diff = unified_diff(&before, &after);
    ProviderNativeExportFilePlan {
        operation_id,
        source,
        file_kind,
        operation_kind,
        target_path: target.display().to_string(),
        backup_path,
        temp_path,
        marker,
        redacted_before: before,
        redacted_after: after,
        redacted_diff: diff,
        rollback_plan: "Restore the Vibex-created backup for this file when a backup exists; created files without a prior backup are not deleted automatically.".to_string(),
        diagnostics: Vec::new(),
        status: if operation_kind == ProviderNativeExportOperationKind::NoOp {
            ProviderNativeExportFileStatus::NoOp
        } else {
            ProviderNativeExportFileStatus::Ready
        },
    }
}

fn blocked_plan(
    export_id: &RequestId,
    source: ProviderNativeExportSource,
    file_kind: ProviderNativeConfigFileKind,
    target: PathBuf,
    reason: impl Into<String>,
) -> ProviderNativeExportFilePlan {
    let reason = reason.into();
    ProviderNativeExportFilePlan {
        operation_id: RequestId::new(),
        source,
        file_kind,
        operation_kind: ProviderNativeExportOperationKind::Blocked,
        target_path: target.display().to_string(),
        backup_path: Some(sibling_path(&target, export_id, "bak")),
        temp_path: Some(sibling_path(&target, export_id, "tmp")),
        marker: None,
        redacted_before: String::new(),
        redacted_after: String::new(),
        redacted_diff: String::new(),
        rollback_plan: "No write will be attempted while this plan is blocked.".to_string(),
        diagnostics: vec![metadata("provider_native_export_blocked", reason)],
        status: ProviderNativeExportFileStatus::Blocked,
    }
}

fn codex_managed_block(profile: &ProviderProfile) -> String {
    let provider_id = format!("vibex_{}", profile.id.as_str().replace('-', "_"));
    let model = profile.default_model.as_deref().unwrap_or("gpt-5");
    let base_url = profile
        .base_url
        .as_deref()
        .unwrap_or("https://api.openai.com/v1");
    let reasoning = profile.reasoning_effort.as_deref().unwrap_or("medium");
    format!(
        "{CODEX_MARKER_START}\n\
         model = \"{model}\"\n\
         model_provider = \"{provider_id}\"\n\
         model_reasoning_effort = \"{reasoning}\"\n\n\
         [model_providers.{provider_id}]\n\
         name = \"{display_name}\"\n\
         base_url = \"{base_url}\"\n\
         env_key = \"OPENAI_API_KEY\"\n\
         wire_api = \"chat\"\n\
         # Plaintext secrets are intentionally not exported by Vibex.\n\
         {CODEX_MARKER_END}\n",
        display_name = escape_toml_string(&profile.display_name),
        model = escape_toml_string(model),
        base_url = escape_toml_string(base_url),
        reasoning = escape_toml_string(reasoning),
        provider_id = provider_id,
    )
}

fn replace_marked_block(current: &str, block: &str) -> Option<String> {
    let start = current.find(CODEX_MARKER_START)?;
    let end_start = current.find(CODEX_MARKER_END)?;
    let end = end_start + CODEX_MARKER_END.len();
    let mut next = String::new();
    next.push_str(&current[..start]);
    next.push_str(block);
    next.push_str(&current[end..]);
    Some(next)
}

fn read_optional(path: &Path) -> VibexResult<Option<String>> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(VibexError::storage(
            "provider_native_export_file_read_failed",
            "failed to read native config file for export preview",
        )
        .with_diagnostic("path", path.display().to_string())
        .with_diagnostic("error", err.to_string())),
    }
}

fn unified_diff(before: &str, after: &str) -> String {
    if before == after {
        return "No changes.".to_string();
    }
    let mut diff = String::from("--- current\n+++ vibex\n");
    for line in before.lines() {
        if !after.lines().any(|candidate| candidate == line) {
            diff.push('-');
            diff.push_str(line);
            diff.push('\n');
        }
    }
    for line in after.lines() {
        if !before.lines().any(|candidate| candidate == line) {
            diff.push('+');
            diff.push_str(line);
            diff.push('\n');
        }
    }
    diff
}

fn sibling_path(target: &Path, export_id: &RequestId, suffix: &str) -> String {
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("native-config");
    target
        .with_file_name(format!(
            "{file_name}.vibex-{}.{}",
            export_id.as_str(),
            suffix
        ))
        .display()
        .to_string()
}

fn target_path(source: ProviderNativeExportSource, roots: NativeExportRoots) -> PathBuf {
    match source {
        ProviderNativeExportSource::Codex => codex_config_root(roots.codex_root),
        ProviderNativeExportSource::Claude => claude_config_root(roots.claude_root),
    }
}

fn target_file_name(source: ProviderNativeExportSource) -> &'static str {
    match source {
        ProviderNativeExportSource::Codex => "config.toml",
        ProviderNativeExportSource::Claude => "settings.json",
    }
}

fn target_file_kind(source: ProviderNativeExportSource) -> ProviderNativeConfigFileKind {
    match source {
        ProviderNativeExportSource::Codex => ProviderNativeConfigFileKind::CodexConfigToml,
        ProviderNativeExportSource::Claude => ProviderNativeConfigFileKind::ClaudeSettingsJson,
    }
}

fn codex_config_root(override_root: Option<PathBuf>) -> PathBuf {
    override_root
        .or_else(|| {
            std::env::var_os("CODEX_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

fn claude_config_root(override_root: Option<PathBuf>) -> PathBuf {
    override_root
        .or_else(|| {
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .unwrap_or_else(|| PathBuf::from(".claude"))
}

fn metadata(key: impl Into<String>, value: impl Into<String>) -> ProviderBindingMetadata {
    ProviderBindingMetadata {
        key: key.into(),
        value: value.into(),
    }
}

fn escape_toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use vibex_core::{
        ProviderNetworkDefaults, ProviderOptions, ProviderPermissionDefaults, ProviderProfileId,
        ProviderProfileStatus, ProviderSandboxDefaults, agent_id_for_provider_kind,
    };

    fn profile(kind: ProviderKind) -> ProviderProfile {
        ProviderProfile {
            id: ProviderProfileId::parse("provider_profile_native_export_test").unwrap(),
            agent_id: agent_id_for_provider_kind(kind),
            kind,
            display_name: "Native Export Test".to_string(),
            status: ProviderProfileStatus::Enabled,
            account_alias: None,
            base_url: Some("https://api.example.test/v1".to_string()),
            default_model: Some("gpt-test".to_string()),
            small_model: None,
            large_model: None,
            configured_models: Vec::new(),
            reasoning_effort: Some("high".to_string()),
            sandbox_defaults: ProviderSandboxDefaults::workspace_write_ask_on_risk(),
            network_defaults: ProviderNetworkDefaults::local_default(),
            permission_defaults: ProviderPermissionDefaults::ask_on_risk(),
            provider_options: ProviderOptions {
                schema_version: 1,
                entries: Vec::new(),
            },
            secrets: Vec::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted_at_ms: None,
        }
    }

    #[test]
    fn native_export_preview_does_not_create_files_or_dirs() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("missing-codex");
        let preview = preview_native_export_with_roots(
            &profile(ProviderKind::Codex),
            ProviderNativeExportPreviewRequest {
                provider_profile_id: ProviderProfileId::parse(
                    "provider_profile_native_export_test",
                )
                .unwrap(),
                source: ProviderNativeExportSource::Codex,
                mode: ProviderNativeExportMode::ProviderProfile,
                persist: false,
            },
            NativeExportRoots {
                codex_root: Some(root.clone()),
                claude_root: None,
            },
        )
        .unwrap();
        assert_eq!(
            preview.files[0].status,
            ProviderNativeExportFileStatus::Ready
        );
        assert!(!root.exists());
        assert!(!PathBuf::from(preview.files[0].temp_path.as_ref().unwrap()).exists());
        assert!(preview.files[0].redacted_diff.contains("OPENAI_API_KEY"));
        assert!(!preview.files[0].redacted_diff.contains("sk-"));
    }

    #[test]
    fn native_export_blocks_unmarked_codex_config() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("codex");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("config.toml"), "model = \"user\"\n").unwrap();
        let preview = preview_native_export_with_roots(
            &profile(ProviderKind::Codex),
            ProviderNativeExportPreviewRequest {
                provider_profile_id: ProviderProfileId::parse(
                    "provider_profile_native_export_test",
                )
                .unwrap(),
                source: ProviderNativeExportSource::Codex,
                mode: ProviderNativeExportMode::ProviderProfile,
                persist: false,
            },
            NativeExportRoots {
                codex_root: Some(root),
                claude_root: None,
            },
        )
        .unwrap();
        assert_eq!(
            preview.files[0].status,
            ProviderNativeExportFileStatus::Blocked
        );
    }

    #[test]
    fn native_export_apply_and_rollback_restores_backup() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("claude");
        fs::create_dir_all(&root).unwrap();
        let settings = root.join("settings.json");
        fs::write(&settings, "{\n  \"theme\": \"dark\"\n}\n").unwrap();
        let preview = preview_native_export_with_roots(
            &profile(ProviderKind::Claude),
            ProviderNativeExportPreviewRequest {
                provider_profile_id: ProviderProfileId::parse(
                    "provider_profile_native_export_test",
                )
                .unwrap(),
                source: ProviderNativeExportSource::Claude,
                mode: ProviderNativeExportMode::ProviderProfile,
                persist: false,
            },
            NativeExportRoots {
                codex_root: None,
                claude_root: Some(root),
            },
        )
        .unwrap();
        let apply = apply_preview(preview.clone());
        assert_eq!(apply.status, ProviderNativeExportApplyStatus::Applied);
        assert!(fs::read_to_string(&settings).unwrap().contains("\"vibex\""));
        let rollback = rollback_preview(preview);
        assert_eq!(
            rollback.status,
            ProviderNativeExportRollbackStatus::Restored
        );
        assert_eq!(
            fs::read_to_string(&settings).unwrap(),
            "{\n  \"theme\": \"dark\"\n}\n"
        );
    }

    #[test]
    fn native_export_updates_only_codex_marked_block() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("codex");
        fs::create_dir_all(&root).unwrap();
        let config = root.join("config.toml");
        fs::write(
            &config,
            "user_key = \"keep\"\n# >>> VIBEX MANAGED PROVIDER EXPORT\nmodel = \"old\"\n# <<< VIBEX MANAGED PROVIDER EXPORT\n",
        )
        .unwrap();
        let preview = preview_native_export_with_roots(
            &profile(ProviderKind::Codex),
            ProviderNativeExportPreviewRequest {
                provider_profile_id: ProviderProfileId::parse(
                    "provider_profile_native_export_test",
                )
                .unwrap(),
                source: ProviderNativeExportSource::Codex,
                mode: ProviderNativeExportMode::ProviderProfile,
                persist: false,
            },
            NativeExportRoots {
                codex_root: Some(root),
                claude_root: None,
            },
        )
        .unwrap();
        assert_eq!(
            preview.files[0].status,
            ProviderNativeExportFileStatus::Ready
        );
        let apply = apply_preview(preview);
        assert_eq!(apply.status, ProviderNativeExportApplyStatus::Applied);
        let written = fs::read_to_string(&config).unwrap();
        assert!(written.contains("user_key = \"keep\""));
        assert!(written.contains("gpt-test"));
        assert!(!written.contains("model = \"old\""));
    }

    #[test]
    fn native_export_failed_temp_write_restores_backup() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("claude");
        fs::create_dir_all(&root).unwrap();
        let settings = root.join("settings.json");
        fs::write(&settings, "{\n  \"theme\": \"dark\"\n}\n").unwrap();
        let mut preview = preview_native_export_with_roots(
            &profile(ProviderKind::Claude),
            ProviderNativeExportPreviewRequest {
                provider_profile_id: ProviderProfileId::parse(
                    "provider_profile_native_export_test",
                )
                .unwrap(),
                source: ProviderNativeExportSource::Claude,
                mode: ProviderNativeExportMode::ProviderProfile,
                persist: false,
            },
            NativeExportRoots {
                codex_root: None,
                claude_root: Some(root.clone()),
            },
        )
        .unwrap();
        let temp_dir_path = root.join("temp-is-directory");
        fs::create_dir_all(&temp_dir_path).unwrap();
        preview.files[0].temp_path = Some(temp_dir_path.display().to_string());

        let apply = apply_preview(preview);
        assert_eq!(
            apply.status,
            ProviderNativeExportApplyStatus::FailedRestored
        );
        assert_eq!(
            apply.files[0].status,
            ProviderNativeExportFileStatus::Restored
        );
        assert_eq!(
            fs::read_to_string(&settings).unwrap(),
            "{\n  \"theme\": \"dark\"\n}\n"
        );
    }
}

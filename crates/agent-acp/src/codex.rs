//! Codex ACP extension, fork and stable runtime-home contracts.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use serde_json::{Value, json};
use vibex_core::{ProviderProfileId, VibexError, VibexResult, VibexSessionId};

use crate::{
    AcpOperation, AcpWireEncoding, CapabilitySource, CapabilitySupport,
    SessionConfigOperationEvidence, ensure_private_runtime_directory,
    write_private_runtime_file_atomic,
};

pub const CODEX_FORK_EXTENSION_VERSION: u32 = 1;

/// Stable native state home. A process fingerprint may select a manifest
/// inside this directory but must never become a path component here.
pub fn codex_acp_runtime_home_path(
    runtime_data: &Path,
    session_id: &VibexSessionId,
    provider_profile_id: &ProviderProfileId,
) -> PathBuf {
    runtime_data
        .join("codex-runtime-homes")
        .join(sanitize_path_component(session_id.as_str()))
        .join(sanitize_path_component(provider_profile_id.as_str()))
}

pub fn prepare_codex_acp_runtime_home(
    runtime_data: &Path,
    session_id: &VibexSessionId,
    provider_profile_id: &ProviderProfileId,
) -> VibexResult<PathBuf> {
    let homes_root = runtime_data.join("codex-runtime-homes");
    let session_root = homes_root.join(sanitize_path_component(session_id.as_str()));
    let runtime_home = session_root.join(sanitize_path_component(provider_profile_id.as_str()));
    ensure_private_runtime_directory(&homes_root)?;
    ensure_private_runtime_directory(&session_root)?;
    ensure_private_runtime_directory(&runtime_home)?;
    Ok(runtime_home)
}

pub fn write_codex_acp_runtime_config(
    runtime_data: &Path,
    session_id: &VibexSessionId,
    provider_profile_id: &ProviderProfileId,
    config: &[u8],
) -> VibexResult<PathBuf> {
    let runtime_home =
        prepare_codex_acp_runtime_home(runtime_data, session_id, provider_profile_id)?;
    let config_path = runtime_home.join("config.toml");
    write_private_runtime_file_atomic(&config_path, config)?;
    Ok(config_path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexForkPlan {
    pub operation: AcpOperation,
    pub encoding: AcpWireEncoding,
    pub source: CapabilitySource,
    pub params: Value,
}

/// Fork is an unstable operation and is available only for exact negotiated
/// identity/generation evidence. Static capability hints never enable it.
pub fn plan_codex_fork(
    evidence: Option<&SessionConfigOperationEvidence>,
    compatibility_identity: &str,
    activation_generation: i64,
    native_session_id: &str,
) -> Option<CodexForkPlan> {
    let evidence = evidence?;
    if evidence.support != CapabilitySupport::Supported
        || !evidence.supported_for(compatibility_identity, activation_generation)
        || evidence.source != CapabilitySource::NegotiatedRuntime
        || evidence.encoding != AcpWireEncoding::VersionedRaw
    {
        return None;
    }
    Some(CodexForkPlan {
        operation: AcpOperation::SessionFork,
        encoding: evidence.encoding,
        source: evidence.source,
        params: json!({
            "sessionId": native_session_id,
            "_meta": { "codexForkVersion": CODEX_FORK_EXTENSION_VERSION }
        }),
    })
}

/// Decodes versioned Codex ACP extension records into the same structured
/// fields consumed by `CodexEventEnricher`. Unknown extensions return `None`.
pub fn decode_codex_extension(
    method: &str,
    params: &Value,
    compatibility_identity: &str,
) -> Option<crate::AgentEventInput> {
    let kind = method.strip_prefix("_codex/")?;
    if !matches!(
        kind,
        "diff" | "command" | "web_search" | "todo" | "collaboration" | "image_generation"
    ) {
        return None;
    }
    let id = first_string(params, &["id", "eventId", "toolCallId"])
        .unwrap_or_else(|| format!("codex-{kind}"));
    let title = first_string(params, &["title", "summary", "query"])
        .unwrap_or_else(|| kind.replace('_', " "));
    let tool_name = match kind {
        "diff" => "diff",
        "command" => "command_execution",
        "todo" => "todo_list",
        other => other,
    };
    Some(crate::AgentEventInput {
        source: crate::AgentEventInputSource::Live,
        compatibility_identity: compatibility_identity.to_string(),
        native_event_id: bounded(id, 160),
        tool_name: tool_name.to_string(),
        title: bounded(title, 512),
        status: vibex_core::ToolCallStatus::Completed,
        raw_input: Some(params.clone()),
        output_summary: first_string(params, &["output", "result", "summary"])
            .map(|value| bounded(value, 8 * 1024)),
        raw_output: None,
        content: params.get("content").cloned(),
        locations: crate::parse_event_locations(params.get("locations")),
        meta: crate::parse_event_meta(params.get("meta")),
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAcpSmokeResult {
    pub command: String,
    pub workspace_path: PathBuf,
    pub prompt: String,
    pub version_output: Option<String>,
    pub started: bool,
}

pub async fn run_codex_agent_acp_smoke(prompt: Option<String>) -> VibexResult<CodexAcpSmokeResult> {
    let workspace_path = crate::resolve_agent_smoke_workspace("codex-acp", "direct")?;
    let command =
        std::env::var("VIBEX_CODEX_ACP_COMMAND").unwrap_or_else(|_| "codex-acp".to_string());
    let mut version_command = Command::new(&command);
    version_command.arg("--version");
    crate::process_environment::sanitize_inherited_appimage_environment(&mut version_command);
    let output = version_command.output().map_err(|error| {
        VibexError::process(
            "codex_acp_binary_missing",
            "Codex ACP adapter was not found",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    if !output.status.success() {
        return Err(VibexError::process(
            "codex_acp_version_probe_failed",
            "Codex ACP adapter version probe failed",
        ));
    }
    Ok(CodexAcpSmokeResult {
        command,
        workspace_path,
        prompt: prompt.unwrap_or_else(|| "Reply with a Codex ACP smoke marker.".to_string()),
        version_output: String::from_utf8(output.stdout)
            .ok()
            .map(|value| bounded(value, 512)),
        started: true,
    })
}

fn sanitize_path_component(value: &str) -> String {
    let value = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let value = value.trim_matches('-');
    if value.is_empty() {
        "unknown".to_string()
    } else {
        value.chars().take(120).collect()
    }
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn bounded(value: String, limit: usize) -> String {
    let value = value.trim();
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AcpOperationStability, AgentEventEnricherKind, normalize_agent_event};

    #[test]
    fn runtime_home_is_stable_across_fingerprints() {
        let root = Path::new("/tmp/vibex-runtime");
        let session = VibexSessionId::new();
        let profile = ProviderProfileId::new();
        let first = codex_acp_runtime_home_path(root, &session, &profile);
        let second = codex_acp_runtime_home_path(root, &session, &profile);
        assert_eq!(first, second);
        assert!(!first.to_string_lossy().contains("sha256"));
    }

    #[cfg(unix)]
    #[test]
    fn runtime_home_and_projected_config_are_owner_only() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::TempDir::new().unwrap();
        let session = VibexSessionId::new();
        let profile = ProviderProfileId::new();
        let home = prepare_codex_acp_runtime_home(temp.path(), &session, &profile).unwrap();
        let config =
            write_codex_acp_runtime_config(temp.path(), &session, &profile, b"model = 'safe'\n")
                .unwrap();

        assert_eq!(
            fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let history = home.join("sessions");
        fs::create_dir(&history).unwrap();
        fs::set_permissions(&history, fs::Permissions::from_mode(0o755)).unwrap();
        prepare_codex_acp_runtime_home(temp.path(), &session, &profile).unwrap();
        assert_eq!(
            fs::metadata(history).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn fork_requires_exact_negotiated_versioned_raw_evidence() {
        let identity = "codex-acp@1";
        let mut evidence = SessionConfigOperationEvidence {
            support: CapabilitySupport::Supported,
            source: CapabilitySource::NegotiatedRuntime,
            encoding: AcpWireEncoding::VersionedRaw,
            stability: AcpOperationStability::VersionedUnstable,
            compatibility_identity: identity.to_string(),
            activation_generation: 2,
        };
        assert!(plan_codex_fork(Some(&evidence), identity, 2, "native-1").is_some());
        evidence.source = CapabilitySource::VersionedRegistry;
        assert!(plan_codex_fork(Some(&evidence), identity, 2, "native-1").is_none());
    }

    #[test]
    fn extension_diff_without_raw_input_keeps_text() {
        let input = decode_codex_extension(
            "_codex/diff",
            &json!({
                "id": "diff-1",
                "content": [{
                    "type": "diff",
                    "path": "src/lib.rs",
                    "kind": "update",
                    "oldText": "before",
                    "newText": "after"
                }]
            }),
            "codex-acp@1",
        )
        .unwrap();
        let events = normalize_agent_event(AgentEventEnricherKind::Codex, &input);
        match &events[0].event {
            crate::CanonicalAgentEvent::FileOperation(file) => {
                assert_eq!(file.old_text.as_deref(), Some("before"));
                assert_eq!(file.new_text.as_deref(), Some("after"));
            }
            other => panic!("expected file operation, got {other:?}"),
        }
    }
}

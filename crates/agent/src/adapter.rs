use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use vibex_core::{
    AgentAuthCatalog, AgentAuthenticateRequest, AgentAuthenticateResult,
    AgentAuthenticationCancelRequest, AgentAuthenticationCompleteRequest,
    AgentCommandDiscoverRequest, AgentCommandDiscoverResponse, AgentCommandExecuteRequest, AgentId,
    AgentLogoutRequest, AgentModelListResponse, AgentModelListSource, AgentSessionConfigProbe,
    AgentSessionSafety, AgentUsageCounterOrigin, AgentUsageExecution, AgentUsageExecutionContext,
    AgentUsageExecutionStatusUpdate, AgentUsageObservation, ElicitationResolution,
    ExternalSessionImportCandidate, MessageAttachment, MessageSubmissionId, PermissionResolution,
    ProviderBinding, ProviderBindingMetadata, ProviderCapabilities, ProviderKind,
    ProviderProfileId, RuntimeBindingId, SessionRuntimeSelection, TimelinePayload,
    TimelineRedactionState, TimelineSource, VibexError, VibexResult, VibexSessionId,
};

#[derive(Debug, Clone)]
pub struct ProviderCreateRequest {
    pub session_id: VibexSessionId,
    pub provider_profile_id: ProviderProfileId,
    pub model: Option<String>,
    pub workspace_root: String,
    pub safety: AgentSessionSafety,
    pub runtime_resources: ProviderRuntimeResources,
}

#[derive(Debug, Clone)]
pub struct ProviderSessionHandle {
    pub binding: ProviderBinding,
    pub capabilities: ProviderCapabilities,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderRuntimeResources {
    pub mcp_servers: Vec<ProviderRuntimeMcpServer>,
    pub skills: Vec<ProviderRuntimeSkill>,
}

impl ProviderRuntimeResources {
    pub fn is_empty(&self) -> bool {
        self.mcp_servers.is_empty() && self.skills.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRuntimeMcpTransport {
    Stdio,
    Sse,
    Http,
}

#[derive(Debug, Clone)]
pub struct ProviderRuntimeMcpServer {
    pub id: String,
    pub display_name: String,
    pub transport: ProviderRuntimeMcpTransport,
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Environment for a stdio server, with resolved secrets already merged
    /// in. Required on the ACP wire.
    pub env: Vec<(String, String)>,
    pub url: Option<String>,
    /// Headers for an HTTP or SSE server, with resolved secrets already merged
    /// in. Required on the ACP wire.
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ProviderRuntimeSkill {
    pub id: String,
    pub display_name: String,
    pub source_uri: Option<String>,
}

#[derive(Clone)]
pub struct ProviderTurnRequest {
    pub session_id: VibexSessionId,
    /// Stable durable submission identity. Adapters may use this as a native
    /// idempotency token only when that capability is explicitly supported.
    pub message_submission_id: Option<MessageSubmissionId>,
    /// Product runtime captured by a durable submission or resolved at
    /// admission for a fenced command such as Continue. When present, the
    /// adapter must use the current committed runtime and fail closed if its
    /// execution fence or effective configuration no longer matches.
    pub required_runtime: Option<SessionRuntimeSelection>,
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
    pub workspace_root: String,
    pub binding: ProviderBinding,
    pub safety: AgentSessionSafety,
    pub runtime_resources: ProviderRuntimeResources,
    pub execution_identity: Option<ProviderTurnExecutionIdentity>,
    pub event_sender: Option<mpsc::UnboundedSender<ProviderEvent>>,
    pub binding_update_sender: Option<mpsc::UnboundedSender<ProviderBinding>>,
    pub usage_execution_context: Option<AgentUsageExecutionContext>,
    pub usage_counter_origin: AgentUsageCounterOrigin,
    pub usage_event_sender: Option<mpsc::UnboundedSender<AgentUsageTelemetryEvent>>,
}

impl fmt::Debug for ProviderTurnRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderTurnRequest")
            .field("session_id", &self.session_id)
            .field(
                "has_message_submission_id",
                &self.message_submission_id.is_some(),
            )
            .field("has_required_runtime", &self.required_runtime.is_some())
            .field("has_text", &!self.text.is_empty())
            .field("attachment_count", &self.attachments.len())
            .field("has_workspace_root", &!self.workspace_root.is_empty())
            .field("has_binding", &true)
            .field(
                "mcp_server_count",
                &self.runtime_resources.mcp_servers.len(),
            )
            .field("skill_count", &self.runtime_resources.skills.len())
            .field("has_execution_identity", &self.execution_identity.is_some())
            .field("has_event_sender", &self.event_sender.is_some())
            .field(
                "has_binding_update_sender",
                &self.binding_update_sender.is_some(),
            )
            .field(
                "has_usage_execution_context",
                &self.usage_execution_context.is_some(),
            )
            .field("usage_counter_origin", &self.usage_counter_origin)
            .field("has_usage_event_sender", &self.usage_event_sender.is_some())
            .finish()
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum AgentUsageTelemetryEvent {
    ExecutionDispatched {
        execution: AgentUsageExecution,
        counter_origin: AgentUsageCounterOrigin,
    },
    Observation(AgentUsageObservation),
    ExecutionStatus(AgentUsageExecutionStatusUpdate),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTurnExecutionIdentity {
    pub binding_id: RuntimeBindingId,
    pub activation_generation: i64,
    /// The concrete model reported by the Agent for this turn. Agent-default
    /// selections may legitimately leave this unknown.
    pub model_id: Option<String>,
}

pub fn legacy_provider_runtime_binding_id(binding: &ProviderBinding) -> RuntimeBindingId {
    RuntimeBindingId::parse(format!(
        "binding_legacy_{}_{}_{}",
        binding.provider_kind,
        binding.session_id.as_str(),
        binding.auth_source.id()
    ))
    .expect("legacy provider binding identity must use the binding_ prefix")
}

pub fn validate_legacy_provider_turn_execution_identity(
    binding: &ProviderBinding,
    identity: &ProviderTurnExecutionIdentity,
) -> VibexResult<()> {
    if identity.binding_id != legacy_provider_runtime_binding_id(binding)
        || identity.activation_generation != 0
    {
        return Err(VibexError::conflict(
            "turn_execution_identity_mismatch",
            "provider turn execution identity no longer matches the prepared binding",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ProviderTurnAttachment {
    pub label: String,
    pub mime_type: Option<String>,
    pub uri: Option<String>,
    pub local_path: Option<PathBuf>,
}

impl ProviderTurnAttachment {
    pub fn is_image(&self) -> bool {
        self.mime_type
            .as_deref()
            .map(|mime_type| mime_type.starts_with("image/"))
            .unwrap_or_else(|| {
                self.local_path
                    .as_ref()
                    .and_then(|path| infer_image_mime_type(path.to_string_lossy().as_ref()))
                    .is_some()
            })
    }
}

pub fn materialize_provider_attachments(
    session_id: &VibexSessionId,
    attachments: &[MessageAttachment],
) -> VibexResult<Vec<ProviderTurnAttachment>> {
    attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| materialize_provider_attachment(session_id, index, attachment))
        .collect()
}

fn materialize_provider_attachment(
    session_id: &VibexSessionId,
    index: usize,
    attachment: &MessageAttachment,
) -> VibexResult<ProviderTurnAttachment> {
    let local_path = match attachment.uri.as_deref() {
        Some(uri) if uri.starts_with("data:") => Some(write_data_url_attachment(
            session_id, index, attachment, uri,
        )?),
        Some(uri) => local_path_from_attachment_uri(uri),
        None => None,
    };

    Ok(ProviderTurnAttachment {
        label: attachment.label.clone(),
        mime_type: attachment.mime_type.clone().or_else(|| {
            local_path
                .as_ref()
                .and_then(|path| infer_image_mime_type(path.to_string_lossy().as_ref()))
        }),
        uri: attachment.uri.clone(),
        local_path,
    })
}

fn write_data_url_attachment(
    session_id: &VibexSessionId,
    index: usize,
    attachment: &MessageAttachment,
    uri: &str,
) -> VibexResult<PathBuf> {
    let Some((metadata, payload)) = uri.split_once(',') else {
        return Err(VibexError::validation(
            "attachment_data_url_invalid",
            "attachment data URL is missing its payload",
        ));
    };
    if !metadata.contains(";base64") {
        return Err(VibexError::validation(
            "attachment_data_url_not_base64",
            "attachment data URL must be base64 encoded before provider use",
        ));
    }

    let bytes = BASE64.decode(payload).map_err(|err| {
        VibexError::validation(
            "attachment_data_url_base64_invalid",
            "attachment data URL payload is invalid",
        )
        .with_diagnostic("error", err.to_string())
    })?;
    let mime_type = attachment
        .mime_type
        .as_deref()
        .or_else(|| {
            metadata
                .strip_prefix("data:")
                .and_then(|value| value.split(';').next())
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("application/octet-stream");
    let extension = extension_for_mime_type(mime_type);
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let digest = hasher.finish();
    let dir = std::env::temp_dir()
        .join("vibex-agent-attachments")
        .join(sanitize_path_segment(session_id.as_str()));
    fs::create_dir_all(&dir).map_err(|err| {
        VibexError::storage(
            "attachment_materialize_dir_failed",
            "failed to create provider attachment directory",
        )
        .with_diagnostic("error", err.to_string())
    })?;
    let path = dir.join(format!(
        "{}-{}-{:016x}.{}",
        index,
        sanitize_path_segment(&attachment.label),
        digest,
        extension
    ));
    fs::write(&path, bytes).map_err(|err| {
        VibexError::storage(
            "attachment_materialize_write_failed",
            "failed to write provider attachment file",
        )
        .with_diagnostic("error", err.to_string())
    })?;
    Ok(path)
}

fn local_path_from_attachment_uri(uri: &str) -> Option<PathBuf> {
    if let Some(path) = uri.strip_prefix("file://") {
        return Some(PathBuf::from(path));
    }

    let path = Path::new(uri);
    path.is_absolute().then(|| path.to_path_buf())
}

fn sanitize_path_segment(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let sanitized: String = sanitized.trim_matches('-').chars().take(80).collect();
    if sanitized.trim().is_empty() {
        "attachment".to_string()
    } else {
        sanitized
    }
}

fn extension_for_mime_type(mime_type: &str) -> &'static str {
    match mime_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        _ => "bin",
    }
}

fn infer_image_mime_type(path: &str) -> Option<String> {
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    let mime_type = if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".gif") {
        "image/gif"
    } else if path.ends_with(".webp") {
        "image/webp"
    } else if path.ends_with(".bmp") {
        "image/bmp"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else {
        return None;
    };
    Some(mime_type.to_string())
}

#[derive(Debug, Clone)]
pub struct ProviderEvent {
    pub source: TimelineSource,
    pub payload: TimelinePayload,
    pub provider_correlation_id: Option<String>,
    pub redaction_state: TimelineRedactionState,
    /// Optional session metadata update. Metadata events are consumed by the
    /// manager and never persisted as timeline items.
    pub session_title: Option<String>,
}

impl ProviderEvent {
    pub fn agent(payload: TimelinePayload) -> Self {
        Self {
            source: TimelineSource::Agent,
            payload,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            session_title: None,
        }
    }

    pub fn provider(payload: TimelinePayload) -> Self {
        Self {
            source: TimelineSource::Provider,
            payload,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            session_title: None,
        }
    }

    pub fn session_title(title: String) -> Self {
        Self {
            source: TimelineSource::Provider,
            payload: TimelinePayload::SystemNotice(vibex_core::SystemNoticePayload {
                level: vibex_core::SystemNoticeLevel::Info,
                message: String::new(),
            }),
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            session_title: Some(title),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderTurnResult {
    pub events: Vec<ProviderEvent>,
    pub binding_update: Option<ProviderBinding>,
    pub completed: bool,
}

#[derive(Debug, Clone)]
pub struct ProviderPermissionResolution {
    pub session_id: VibexSessionId,
    pub binding: ProviderBinding,
    pub resolution: PermissionResolution,
}

#[derive(Debug, Clone)]
pub struct ProviderElicitationResolution {
    pub session_id: VibexSessionId,
    pub binding: ProviderBinding,
    pub execution_identity: ProviderTurnExecutionIdentity,
    pub resolution: ElicitationResolution,
}

#[async_trait]
pub trait AgentProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    fn capabilities(&self) -> ProviderCapabilities;

    fn capabilities_for_profile(
        &self,
        _provider_profile_id: Option<&ProviderProfileId>,
    ) -> ProviderCapabilities {
        self.capabilities()
    }

    async fn list_auth_methods(
        &self,
        _agent_id: &AgentId,
        _provider_profile_id: Option<&ProviderProfileId>,
    ) -> VibexResult<AgentAuthCatalog> {
        Err(VibexError::capability(
            "agent_auth_discovery_unsupported",
            "authentication discovery is not supported by this provider",
        ))
    }

    async fn authenticate_agent(
        &self,
        _request: AgentAuthenticateRequest,
    ) -> VibexResult<AgentAuthenticateResult> {
        Err(VibexError::capability(
            "agent_authenticate_unsupported",
            "authentication is not supported by this provider",
        ))
    }

    async fn cancel_agent_authentication(
        &self,
        _request: AgentAuthenticationCancelRequest,
    ) -> VibexResult<bool> {
        Err(VibexError::capability(
            "agent_authentication_cancel_unsupported",
            "cancelling authentication is not supported by this provider",
        ))
    }

    async fn complete_agent_authentication(
        &self,
        _request: AgentAuthenticationCompleteRequest,
    ) -> VibexResult<bool> {
        Err(VibexError::capability(
            "agent_authentication_complete_unsupported",
            "completing interactive authentication is not supported by this provider",
        ))
    }

    async fn logout_agent(&self, _request: AgentLogoutRequest) -> VibexResult<()> {
        Err(VibexError::capability(
            "agent_logout_unsupported",
            "logout is not supported by this provider",
        ))
    }

    async fn list_models(
        &self,
        provider_profile_id: Option<&ProviderProfileId>,
    ) -> VibexResult<AgentModelListResponse> {
        Ok(AgentModelListResponse {
            agent_id: None,
            provider_kind: self.kind(),
            provider_profile_id: provider_profile_id.cloned(),
            models: Vec::new(),
            reasoning_efforts: Vec::new(),
            model_capabilities: Vec::new(),
            source: AgentModelListSource::Unavailable,
            diagnostics: vec![ProviderBindingMetadata {
                key: "modelList".to_string(),
                value: "unavailable".to_string(),
            }],
        })
    }

    /// Stateless probe for session-level configuration choices (modes,
    /// reasoning efforts). Providers without discovery return empty evidence;
    /// callers apply their own fallbacks.
    async fn probe_session_config(
        &self,
        _provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<AgentSessionConfigProbe> {
        Ok(AgentSessionConfigProbe::default())
    }

    /// Stateless session-config probe for one concrete model. Providers that
    /// do not expose model-sensitive options may reuse their Profile probe.
    async fn probe_session_config_for_model(
        &self,
        provider_profile_id: &ProviderProfileId,
        _model_id: &str,
    ) -> VibexResult<AgentSessionConfigProbe> {
        self.probe_session_config(provider_profile_id).await
    }

    /// Stateless Agent-level probe used during Agent setup. Unlike
    /// `probe_session_config`, this path deliberately has no Provider Profile
    /// and therefore must not resolve credentials, models, or provider
    /// projections.
    async fn probe_agent_session_config(
        &self,
        _agent_id: &AgentId,
    ) -> VibexResult<AgentSessionConfigProbe> {
        Ok(AgentSessionConfigProbe::default())
    }

    async fn create_session(
        &self,
        request: ProviderCreateRequest,
    ) -> VibexResult<ProviderSessionHandle>;

    async fn resume_session(&self, binding: ProviderBinding) -> VibexResult<ProviderSessionHandle>;

    async fn prepare_turn_execution(
        &self,
        _handle: &ProviderSessionHandle,
        _request: &ProviderTurnRequest,
    ) -> VibexResult<Option<ProviderTurnExecutionIdentity>> {
        Ok(None)
    }

    async fn list_import_candidates(
        &self,
        _provider_profile_id: &ProviderProfileId,
        _workspace_root: Option<&str>,
    ) -> VibexResult<Vec<ExternalSessionImportCandidate>> {
        Err(VibexError::capability(
            "provider_native_session_list_unsupported",
            "this provider does not support native session listing",
        ))
    }

    async fn import_session(
        &self,
        _request: ProviderCreateRequest,
        _candidate: ExternalSessionImportCandidate,
    ) -> VibexResult<ProviderSessionHandle> {
        Err(VibexError::capability(
            "provider_native_session_import_unsupported",
            "this provider does not support native session import",
        ))
    }

    async fn send_turn(
        &self,
        handle: ProviderSessionHandle,
        request: ProviderTurnRequest,
    ) -> VibexResult<ProviderTurnResult>;

    async fn discover_commands(
        &self,
        _request: AgentCommandDiscoverRequest,
    ) -> VibexResult<AgentCommandDiscoverResponse> {
        Ok(AgentCommandDiscoverResponse {
            entries: Vec::new(),
            diagnostics: Vec::new(),
        })
    }

    async fn execute_command(
        &self,
        _handle: ProviderSessionHandle,
        _request: AgentCommandExecuteRequest,
        _turn: ProviderTurnRequest,
    ) -> VibexResult<ProviderTurnResult> {
        Err(VibexError::capability(
            "provider_command_execution_unsupported",
            "this provider does not support command execution",
        ))
    }

    async fn interrupt(&self, _handle: ProviderSessionHandle) -> VibexResult<()> {
        Err(VibexError::capability(
            "interrupt_unsupported",
            "this provider does not support interrupt",
        ))
    }

    async fn resolve_permission(&self, _request: ProviderPermissionResolution) -> VibexResult<()> {
        Err(VibexError::capability(
            "permission_resolution_unsupported",
            "this provider does not support permission resolution callbacks",
        ))
    }

    async fn resolve_elicitation(
        &self,
        _request: ProviderElicitationResolution,
    ) -> VibexResult<()> {
        Err(VibexError::capability(
            "elicitation_resolution_unsupported",
            "this provider does not support elicitation callbacks",
        ))
    }

    /// Releases provider-side runtime resources for a session (for example a
    /// long-lived ACP agent process). Called when a session is archived or
    /// deleted; providers without long-lived runtime state can keep the
    /// default no-op.
    async fn close_session(&self, _binding: ProviderBinding) -> VibexResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_turn_request_debug_omits_prompt_and_runtime_secrets() {
        let session_id = VibexSessionId::new();
        let profile_id = ProviderProfileId::parse("provider_debug_test").unwrap();
        let request = ProviderTurnRequest {
            session_id: session_id.clone(),
            message_submission_id: Some(MessageSubmissionId::new()),
            required_runtime: None,
            text: "prompt-secret-SHOULD-NOT-DEBUG".to_string(),
            attachments: vec![MessageAttachment {
                label: "secret-attachment".to_string(),
                mime_type: Some("text/plain".to_string()),
                uri: Some("file:///private/secret.txt".to_string()),
                inline_text_offset: None,
            }],
            workspace_root: "/private/workspace".to_string(),
            binding: ProviderBinding {
                session_id,
                provider_kind: ProviderKind::Acp,
                auth_source: vibex_core::RuntimeAuthSource::provider_profile(profile_id),
                auth_source_revision: 1,
                native: vibex_core::ProviderNativeBinding {
                    native_session_id: Some("native-secret-id".to_string()),
                    native_thread_id: None,
                    native_resume_token: Some("resume-secret-token".to_string()),
                    session_config_state: None,
                    redacted_metadata: Vec::new(),
                },
                created_at_ms: 0,
                updated_at_ms: 0,
            },
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            runtime_resources: ProviderRuntimeResources::default(),
            execution_identity: None,
            event_sender: None,
            binding_update_sender: None,
            usage_execution_context: None,
            usage_counter_origin: AgentUsageCounterOrigin::Unknown,
            usage_event_sender: None,
        };

        let debug = format!("{request:?}");
        for sensitive in [
            "prompt-secret-SHOULD-NOT-DEBUG",
            "secret-attachment",
            "/private/secret.txt",
            "/private/workspace",
            "native-secret-id",
            "resume-secret-token",
        ] {
            assert!(!debug.contains(sensitive));
        }
    }

    #[test]
    fn materializes_data_url_attachments_to_local_files() {
        let session_id = VibexSessionId::new();
        let attachment = MessageAttachment {
            label: "pasted image.png".to_string(),
            mime_type: Some("image/png".to_string()),
            uri: Some("data:image/png;base64,aGVsbG8=".to_string()),
            inline_text_offset: Some(6),
        };

        let materialized =
            materialize_provider_attachments(&session_id, std::slice::from_ref(&attachment))
                .unwrap();

        assert_eq!(materialized.len(), 1);
        let path = materialized[0].local_path.as_ref().unwrap();
        assert!(path.exists());
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("png")
        );
        assert_eq!(std::fs::read(path).unwrap(), b"hello");
        assert_eq!(materialized[0].label, attachment.label);
        assert_eq!(materialized[0].mime_type.as_deref(), Some("image/png"));
        assert!(materialized[0].is_image());

        let _ = std::fs::remove_file(path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[test]
    fn materializes_absolute_image_paths_without_copying() {
        let session_id = VibexSessionId::new();
        let attachment = MessageAttachment {
            label: "screenshot".to_string(),
            mime_type: None,
            uri: Some("/tmp/vibex-screenshot.PNG".to_string()),
            inline_text_offset: None,
        };

        let materialized = materialize_provider_attachments(&session_id, &[attachment]).unwrap();

        assert_eq!(
            materialized[0].local_path.as_deref(),
            Some(Path::new("/tmp/vibex-screenshot.PNG"))
        );
        assert_eq!(materialized[0].mime_type.as_deref(), Some("image/png"));
        assert!(materialized[0].is_image());
    }
}

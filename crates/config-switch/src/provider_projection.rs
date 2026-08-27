//! Deterministic, version-gated Agent/provider projection.
//!
//! Planning is display-safe and performs no Secret or filesystem IO. Secret
//! resolution and private overlay materialization happen together immediately
//! before an Agent process is prepared.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use vibex_core::{
    AgentConfiguredModelBinding, AgentCredential, AgentCredentialControl, AgentCredentialKind,
    AgentCredentialStatus, AgentModelControl, AgentModelProviderBinding,
    AgentModelProviderBindingCreateRequest, AgentModelProviderBindingId,
    AgentModelProviderBindingListRequest, AgentModelProviderBindingStatus,
    AgentModelProviderBindingUpdateRequest, AgentProviderControl,
    AgentProviderProjectionCapability, AgentProviderProjectionCapabilityRequest,
    AgentProviderProjectionDescriptor, AgentProviderProjectionPlan, AgentProviderProjectionPreview,
    AgentProviderProjectionPreviewRequest, AgentProviderProjectionRegistry, AgentRuntimeProfile,
    AgentRuntimeProfileCreateRequest, AgentRuntimeProfileId, AgentRuntimeProfileUpdateRequest,
    ConfigOverlayStrategy, ManagedProjectionOverlay, ModelProviderCredentialReference,
    ModelProviderEndpoint, ModelProviderHeaderValue, ModelProviderProfile,
    ModelProviderProfileCreateRequest, ModelProviderProfileId, ModelProviderProfileUpdateRequest,
    ModelProviderProxyPolicy, ProjectionAuthState, ProjectionDescriptorMatch,
    ProjectionEvidenceState, ProjectionOverlayPreview, ProjectionSecretEnvReference,
    ProjectionTargetKind, ProjectionTargetPreview, ProjectionVerificationState,
    ProviderBindingMetadata, ProviderCredentialSecretMutationRequest, ProviderModelCapabilities,
    ProviderSecretBackend, ProviderSecretSetupState, ProviderSwitchBehavior, RequestId, VibexError,
    VibexResult, unix_timestamp_ms,
};
use vibex_db::{
    AgentModelProviderBindingRepository, AgentRuntimeProfileRepository,
    ModelProviderProfileRepository, ProviderProjectionCompatibilityRepository,
};

use super::{ProviderConfigService, secrets};

const MAX_WORKSPACE_KEY_LEN: usize = 192;
const PROJECTION_FINGERPRINT_DOMAIN: &str = "vibex/provider-projection-plan/v1";
const PROJECTION_RUNTIME_DIR: &str = "provider-projections";
const OPENCODE_CONFIG_ENV: &str = "OPENCODE_CONFIG_CONTENT";
const OPENCODE_SECRET_ENV: &str = "VIBEX_OPENCODE_PROVIDER_API_KEY";
const CODEX_MODEL_PROVIDER_ENV: &str = "MODEL_PROVIDER";
const CODEX_DEFAULT_AUTH_REQUEST_ENV: &str = "DEFAULT_AUTH_REQUEST";
const CODEX_DEFAULT_API_KEY_AUTH_REQUEST: &str = r#"{"methodId":"api-key"}"#;
const CODEX_PROVIDER_ORIGINATOR: &str = "codex_cli_rs";
const CLAUDE_AGENT_ID: &str = "claude";
const CLAUDE_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
const CLAUDE_AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
const OVERLAY_SECRET_PLACEHOLDER_PREFIX: &str = "__VIBEX_SECRET_ENV_";
const OVERLAY_SECRET_PLACEHOLDER_SUFFIX: &str = "__";

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedProjectionSecret(String);

impl ResolvedProjectionSecret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResolvedProjectionSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedProjectionSecretEnv {
    pub key: String,
    pub value: ResolvedProjectionSecret,
}

impl fmt::Debug for ResolvedProjectionSecretEnv {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedProjectionSecretEnv")
            .field("key", &self.key)
            .field("value", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedAgentProviderProjection {
    pub binding_id: AgentModelProviderBindingId,
    pub non_secret_env: BTreeMap<String, String>,
    pub secret_env: Vec<ResolvedProjectionSecretEnv>,
    pub overlay_root: PathBuf,
    pub overlay_files: Vec<PathBuf>,
    pub process_args: Vec<String>,
    pub session_config: Vec<ProviderBindingMetadata>,
    pub effective_model: Option<String>,
    pub switch_behavior: ProviderSwitchBehavior,
    pub fingerprint: String,
}

/// Stable, non-secret launch identity for a legacy provider profile binding.
/// A missing compatibility binding is represented as `None` by the service
/// method that creates this value, so ordinary ACP profiles retain their
/// existing environment-only launch path.
#[derive(Clone)]
pub struct LegacyAgentProviderProjectionRuntimePlan {
    pub plan: AgentProviderProjectionPlan,
    pub non_secret_env: BTreeMap<String, String>,
    pub process_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyAgentProviderModelIdProjection {
    pub product_model_id: String,
    pub runtime_model_id: String,
}

impl ResolvedAgentProviderProjection {
    pub fn child_environment(&self) -> Vec<(String, String)> {
        self.non_secret_env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .chain(
                self.secret_env
                    .iter()
                    .map(|entry| (entry.key.clone(), entry.value.expose().to_string())),
            )
            .collect()
    }
}

impl fmt::Debug for ResolvedAgentProviderProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedAgentProviderProjection")
            .field("binding_id", &self.binding_id)
            .field(
                "non_secret_env_keys",
                &self.non_secret_env.keys().collect::<Vec<_>>(),
            )
            .field(
                "secret_env_keys",
                &self
                    .secret_env
                    .iter()
                    .map(|entry| entry.key.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("overlay_file_count", &self.overlay_files.len())
            .field("process_arg_count", &self.process_args.len())
            .field("effective_model", &self.effective_model)
            .field("switch_behavior", &self.switch_behavior)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AgentProviderProjectionEngine;

impl AgentProviderProjectionEngine {
    /// Resolve the non-secret portion of a projection without materializing
    /// overlay files or looking up credentials. Process snapshots use this
    /// same calculation as the eventual launch so their fingerprint remains
    /// stable across acquisition and spawn.
    pub fn non_secret_environment_for_plan(
        plan: &AgentProviderProjectionPlan,
        runtime_root: &Path,
        workspace_key: &str,
    ) -> VibexResult<BTreeMap<String, String>> {
        validate_workspace_key(workspace_key)?;
        let overlay_root = projection_overlay_root(runtime_root, &plan.binding_id, workspace_key);
        let mut non_secret_env = plan.non_secret_env.clone();
        if let Some(key) = plan.runtime_home_env_key.as_deref() {
            non_secret_env.insert(
                key.to_string(),
                runtime_home_env_value(key, &overlay_root, &plan.overlay_files),
            );
        }
        Ok(non_secret_env)
    }

    /// Resolve launch argument templates without resolving credentials or
    /// writing files. The returned paths are deterministic for a binding and
    /// workspace, so process snapshots and actual launches share one identity.
    pub fn process_args_for_plan(
        plan: &AgentProviderProjectionPlan,
        runtime_root: &Path,
        workspace_key: &str,
    ) -> VibexResult<Vec<String>> {
        validate_workspace_key(workspace_key)?;
        let overlay_root = projection_overlay_root(runtime_root, &plan.binding_id, workspace_key);
        Ok(process_args_for_overlay_root(
            &plan.process_args,
            &overlay_root,
        ))
    }

    pub fn plan(
        model_provider: &ModelProviderProfile,
        runtime: &AgentRuntimeProfile,
        binding: &AgentModelProviderBinding,
        descriptor: &AgentProviderProjectionDescriptor,
        workspace_key: &str,
    ) -> VibexResult<AgentProviderProjectionPlan> {
        validate_workspace_key(workspace_key)?;
        model_provider.validate()?;
        runtime.validate()?;
        binding.validate()?;
        descriptor.validate()?;
        if binding.agent_id != runtime.version_identity.route.agent_id
            || binding.runtime_profile_id != runtime.id
            || binding.model_provider_profile_id != model_provider.id
            || binding.projection_descriptor_id != descriptor.id
            || binding.agent_id != descriptor.route.agent_id
            || runtime.version_identity.route != descriptor.route
        {
            return Err(VibexError::validation(
                "agent_projection_input_mismatch",
                "projection input entities do not describe the same Agent binding",
            ));
        }
        if !descriptor.model_interfaces.is_empty() {
            binding.validate_against_descriptor(descriptor)?;
        }

        let selected_model = selected_model(model_provider, binding)?;
        let endpoint = selected_endpoint(model_provider, binding, selected_model)?;
        let credential = selected_credential(model_provider, binding)?;
        if let Some(credential) = credential
            && !descriptor
                .credential_kinds
                .contains(&credential.credential.kind())
            && !descriptor.credential_kinds.is_empty()
        {
            return Err(VibexError::validation(
                "agent_projection_credential_kind_unsupported",
                "selected credential kind is not supported by the selected projection descriptor",
            ));
        }
        let effective_model = selected_model
            .map(|model| projected_runtime_model_id(model_provider, descriptor, model));

        let mut non_secret_env = binding.projection_overrides.non_secret_env.clone();
        let mut secret_env = Vec::new();
        let mut overlays = Vec::new();
        let mut session_config = Vec::new();
        let mut targets = Vec::new();
        let mut diagnostics = Vec::new();

        project_provider_control(
            descriptor,
            model_provider,
            binding,
            endpoint,
            selected_model,
            &mut non_secret_env,
            &mut overlays,
            &mut session_config,
            &mut targets,
            &mut diagnostics,
        )?;
        project_credential_control(
            descriptor,
            credential,
            &mut secret_env,
            &mut targets,
            &mut diagnostics,
        )?;
        project_model_control(
            descriptor,
            selected_model,
            &mut non_secret_env,
            &mut session_config,
            &mut targets,
        );
        apply_agent_projection_defaults(
            descriptor,
            model_provider,
            selected_model,
            endpoint,
            &mut non_secret_env,
        );

        let runtime_home_env_key =
            private_home_env_key(descriptor.route.agent_id.as_str()).map(ToOwned::to_owned);
        let process_args = projection_process_arg_templates(descriptor.route.agent_id.as_str());

        if let ModelProviderProxyPolicy::Endpoint(proxy) = &model_provider.proxy_policy {
            non_secret_env.insert("HTTPS_PROXY".to_string(), proxy.clone());
            targets.push(ProjectionTargetPreview {
                field: "proxy".to_string(),
                target_kind: ProjectionTargetKind::Environment,
                target: "HTTPS_PROXY".to_string(),
                value_preview: redact_endpoint(proxy),
                secret: false,
            });
        }
        let header_names = model_provider
            .headers
            .iter()
            .map(|header| header.name.trim())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        if !header_names.is_empty() {
            session_config.push(ProviderBindingMetadata {
                key: "headerNames".to_string(),
                value: header_names.join(","),
            });
            targets.push(ProjectionTargetPreview {
                field: "headers".to_string(),
                target_kind: ProjectionTargetKind::AcpConfigOption,
                target: "provider request headers".to_string(),
                value_preview: format!("{} configured", header_names.len()),
                secret: model_provider.headers.iter().any(|header| {
                    matches!(header.value, ModelProviderHeaderValue::SecretReference(_))
                }),
            });
        }

        let fingerprint = projection_fingerprint(
            model_provider,
            runtime,
            binding,
            descriptor,
            selected_model,
            &non_secret_env,
            &secret_env,
            &overlays,
            runtime_home_env_key.as_deref(),
            &process_args,
        )?;
        let overlay_previews = overlays
            .iter()
            .map(|overlay| ProjectionOverlayPreview {
                relative_path: overlay.relative_path.clone(),
                format: overlay.format.clone(),
                contains_secret_reference: overlay.contains_secret_reference,
            })
            .collect();
        let preview = AgentProviderProjectionPreview {
            schema_version: vibex_core::PROVIDER_PROJECTION_SCHEMA_VERSION,
            binding_id: binding.id.clone(),
            descriptor_id: descriptor.id.clone(),
            descriptor_version: descriptor.descriptor_version.clone(),
            evidence_state: descriptor.evidence.state,
            command_summary: command_summary(runtime),
            targets,
            overlay_files: overlay_previews,
            effective_model: effective_model.clone(),
            switch_behavior: descriptor.switch_behavior,
            projection_fingerprint: fingerprint.clone(),
            diagnostics: diagnostics.clone(),
        };
        Ok(AgentProviderProjectionPlan {
            binding_id: binding.id.clone(),
            descriptor_id: descriptor.id.clone(),
            non_secret_env,
            secret_env,
            overlay_files: overlays,
            runtime_home_env_key,
            process_args,
            session_config,
            effective_model,
            switch_behavior: descriptor.switch_behavior,
            fingerprint,
            preview,
            diagnostics,
        })
    }

    pub fn resolve_and_materialize(
        plan: &AgentProviderProjectionPlan,
        runtime_root: &Path,
        workspace_key: &str,
    ) -> VibexResult<ResolvedAgentProviderProjection> {
        validate_workspace_key(workspace_key)?;
        let mut secret_env = Vec::with_capacity(plan.secret_env.len());
        for reference in &plan.secret_env {
            let value = secrets::resolve_provider_secret_reference(
                reference.secret_reference.backend,
                reference.secret_reference.setup_state,
                &reference.secret_reference.lookup_key,
            )?
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_projection_secret_missing",
                    "required provider credential is not available",
                )
                .with_diagnostic("credentialId", reference.credential_id.as_str())
                .with_diagnostic("target", reference.key.as_str())
            })?;
            secret_env.push(ResolvedProjectionSecretEnv {
                key: reference.key.clone(),
                value: ResolvedProjectionSecret(value),
            });
        }

        ensure_private_directory(runtime_root)?;
        let binding_root = runtime_root.join(plan.binding_id.as_str());
        ensure_private_directory(&binding_root)?;
        let overlay_root = projection_overlay_root(runtime_root, &plan.binding_id, workspace_key);
        ensure_private_directory(&overlay_root)?;
        let mut overlay_files = Vec::with_capacity(plan.overlay_files.len());
        for overlay in &plan.overlay_files {
            let path = safe_overlay_path(&overlay_root, &overlay.relative_path)?;
            let content = materialize_overlay_secrets(overlay, &secret_env)?;
            write_private_file_atomic(&path, content.as_bytes())?;
            overlay_files.push(path);
        }
        let non_secret_env =
            Self::non_secret_environment_for_plan(plan, runtime_root, workspace_key)?;
        let process_args = process_args_for_overlay_root(&plan.process_args, &overlay_root);
        Ok(ResolvedAgentProviderProjection {
            binding_id: plan.binding_id.clone(),
            non_secret_env,
            secret_env,
            overlay_root,
            overlay_files,
            process_args,
            session_config: plan.session_config.clone(),
            effective_model: plan.effective_model.clone(),
            switch_behavior: plan.switch_behavior,
            fingerprint: plan.fingerprint.clone(),
        })
    }
}

fn overlay_secret_placeholder(secret_env_key: &str) -> String {
    format!(
        "{OVERLAY_SECRET_PLACEHOLDER_PREFIX}{secret_env_key}{OVERLAY_SECRET_PLACEHOLDER_SUFFIX}"
    )
}

fn materialize_overlay_secrets(
    overlay: &ManagedProjectionOverlay,
    secrets: &[ResolvedProjectionSecretEnv],
) -> VibexResult<String> {
    if !overlay.contains_secret_reference
        || !overlay.content.contains(OVERLAY_SECRET_PLACEHOLDER_PREFIX)
    {
        return Ok(overlay.content.clone());
    }
    let content = match overlay.format.as_str() {
        "yaml" => {
            let mut value =
                serde_yaml::from_str::<serde_yaml::Value>(&overlay.content).map_err(|error| {
                    VibexError::validation(
                        "agent_projection_overlay_secret_decode_failed",
                        "managed overlay could not be decoded before Secret materialization",
                    )
                    .with_diagnostic("format", overlay.format.as_str())
                    .with_diagnostic("error", error.to_string())
                })?;
            for secret in secrets {
                replace_yaml_scalar(
                    &mut value,
                    &overlay_secret_placeholder(&secret.key),
                    secret.value.expose(),
                );
            }
            serde_yaml::to_string(&value).map_err(|error| {
                VibexError::validation(
                    "agent_projection_overlay_secret_encode_failed",
                    "managed overlay could not be encoded after Secret materialization",
                )
                .with_diagnostic("format", overlay.format.as_str())
                .with_diagnostic("error", error.to_string())
            })?
        }
        "toml" => {
            let mut value = toml::from_str::<toml::Value>(&overlay.content).map_err(|error| {
                VibexError::validation(
                    "agent_projection_overlay_secret_decode_failed",
                    "managed overlay could not be decoded before Secret materialization",
                )
                .with_diagnostic("format", overlay.format.as_str())
                .with_diagnostic("error", error.to_string())
            })?;
            for secret in secrets {
                replace_toml_scalar(
                    &mut value,
                    &overlay_secret_placeholder(&secret.key),
                    secret.value.expose(),
                );
            }
            toml::to_string(&value).map_err(|error| {
                VibexError::validation(
                    "agent_projection_overlay_secret_encode_failed",
                    "managed overlay could not be encoded after Secret materialization",
                )
                .with_diagnostic("format", overlay.format.as_str())
                .with_diagnostic("error", error.to_string())
            })?
        }
        "json" => {
            let mut value =
                serde_json::from_str::<serde_json::Value>(&overlay.content).map_err(|error| {
                    VibexError::validation(
                        "agent_projection_overlay_secret_decode_failed",
                        "managed overlay could not be decoded before Secret materialization",
                    )
                    .with_diagnostic("format", overlay.format.as_str())
                    .with_diagnostic("error", error.to_string())
                })?;
            for secret in secrets {
                replace_json_scalar(
                    &mut value,
                    &overlay_secret_placeholder(&secret.key),
                    secret.value.expose(),
                );
            }
            serde_json::to_string(&value).map_err(|error| {
                VibexError::validation(
                    "agent_projection_overlay_secret_encode_failed",
                    "managed overlay could not be encoded after Secret materialization",
                )
                .with_diagnostic("format", overlay.format.as_str())
                .with_diagnostic("error", error.to_string())
            })?
        }
        _ => {
            return Err(VibexError::capability(
                "agent_projection_overlay_secret_format_unsupported",
                "managed overlay Secret materialization is not supported for this format",
            ));
        }
    };
    if content.contains(OVERLAY_SECRET_PLACEHOLDER_PREFIX) {
        return Err(VibexError::validation(
            "agent_projection_overlay_secret_missing",
            "managed overlay references a Secret that was not resolved",
        ));
    }
    Ok(content)
}

fn replace_json_scalar(value: &mut serde_json::Value, marker: &str, replacement: &str) {
    match value {
        serde_json::Value::String(candidate) if candidate == marker => {
            *candidate = replacement.to_string();
        }
        serde_json::Value::Array(values) => {
            for value in values {
                replace_json_scalar(value, marker, replacement);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                replace_json_scalar(value, marker, replacement);
            }
        }
        _ => {}
    }
}

fn replace_toml_scalar(value: &mut toml::Value, marker: &str, replacement: &str) {
    match value {
        toml::Value::String(candidate) if candidate == marker => {
            *candidate = replacement.to_string();
        }
        toml::Value::Array(values) => {
            for value in values {
                replace_toml_scalar(value, marker, replacement);
            }
        }
        toml::Value::Table(values) => {
            for (_, value) in values.iter_mut() {
                replace_toml_scalar(value, marker, replacement);
            }
        }
        _ => {}
    }
}

fn replace_yaml_scalar(value: &mut serde_yaml::Value, marker: &str, replacement: &str) {
    match value {
        serde_yaml::Value::String(candidate) if candidate == marker => {
            *candidate = replacement.to_string();
        }
        serde_yaml::Value::Sequence(values) => {
            for value in values {
                replace_yaml_scalar(value, marker, replacement);
            }
        }
        serde_yaml::Value::Mapping(values) => {
            for value in values.values_mut() {
                replace_yaml_scalar(value, marker, replacement);
            }
        }
        serde_yaml::Value::Tagged(tagged) => {
            replace_yaml_scalar(&mut tagged.value, marker, replacement);
        }
        _ => {}
    }
}

impl ProviderConfigService {
    pub fn list_model_provider_profiles(&self) -> VibexResult<Vec<ModelProviderProfile>> {
        let conn = self.open_connection()?;
        ModelProviderProfileRepository::list(&conn)
    }

    pub fn create_model_provider_profile(
        &self,
        request: ModelProviderProfileCreateRequest,
    ) -> VibexResult<ModelProviderProfile> {
        let now = unix_timestamp_ms().max(1);
        let mut credentials = request.credentials;
        for credential in &mut credentials {
            credential.revision = credential.revision.max(1);
        }
        let profile = ModelProviderProfile {
            id: ModelProviderProfileId::new(),
            legacy_provider_profile_id: None,
            display_name: request.display_name,
            vendor_hint: request.vendor_hint,
            endpoints: request.endpoints,
            proxy_policy: request.proxy_policy,
            credentials,
            configured_models: request.configured_models,
            default_model_id: request.default_model_id,
            headers: request.headers,
            status: request.status,
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };
        let conn = self.open_connection()?;
        ModelProviderProfileRepository::insert(&conn, &profile)?;
        Ok(profile)
    }

    pub fn update_model_provider_profile(
        &self,
        request: ModelProviderProfileUpdateRequest,
    ) -> VibexResult<ModelProviderProfile> {
        let mut profile = request.profile;
        profile.updated_at_ms = unix_timestamp_ms().max(profile.updated_at_ms.saturating_add(1));
        let conn = self.open_connection()?;
        let updated =
            ModelProviderProfileRepository::update(&conn, &profile, request.expected_revision)?;
        self.refresh_bindings_for_model_provider(&conn, &updated.id)?;
        Ok(updated)
    }

    pub fn list_agent_runtime_profiles(
        &self,
        agent_id: &vibex_core::AgentId,
    ) -> VibexResult<Vec<AgentRuntimeProfile>> {
        let conn = self.open_connection()?;
        AgentRuntimeProfileRepository::list_for_agent(&conn, agent_id)
    }

    pub fn create_agent_runtime_profile(
        &self,
        request: AgentRuntimeProfileCreateRequest,
    ) -> VibexResult<AgentRuntimeProfile> {
        let now = unix_timestamp_ms().max(1);
        let profile = AgentRuntimeProfile {
            id: AgentRuntimeProfileId::new(),
            legacy_provider_profile_id: None,
            version_identity: request.version_identity,
            command: request.command,
            args: request.args,
            safe_env_references: request.safe_env_references,
            cwd_template: request.cwd_template,
            process_strategy: request.process_strategy,
            runtime_home_strategy: request.runtime_home_strategy,
            host_capabilities: request.host_capabilities,
            resource_policy: request.resource_policy,
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };
        let conn = self.open_connection()?;
        AgentRuntimeProfileRepository::insert(&conn, &profile)?;
        Ok(profile)
    }

    pub fn update_agent_runtime_profile(
        &self,
        request: AgentRuntimeProfileUpdateRequest,
    ) -> VibexResult<AgentRuntimeProfile> {
        let mut profile = request.profile;
        profile.updated_at_ms = unix_timestamp_ms().max(profile.updated_at_ms.saturating_add(1));
        let conn = self.open_connection()?;
        let updated =
            AgentRuntimeProfileRepository::update(&conn, &profile, request.expected_revision)?;
        self.refresh_bindings_for_runtime(&conn, &updated.id)?;
        Ok(updated)
    }

    pub fn list_agent_model_provider_bindings(
        &self,
        request: AgentModelProviderBindingListRequest,
    ) -> VibexResult<Vec<AgentModelProviderBinding>> {
        let conn = self.open_connection()?;
        match (request.agent_id, request.model_provider_profile_id) {
            (Some(agent_id), Some(provider_id)) => Ok(
                AgentModelProviderBindingRepository::list_for_agent(&conn, &agent_id)?
                    .into_iter()
                    .filter(|binding| binding.model_provider_profile_id == provider_id)
                    .collect(),
            ),
            (Some(agent_id), None) => {
                AgentModelProviderBindingRepository::list_for_agent(&conn, &agent_id)
            }
            (None, Some(provider_id)) => {
                AgentModelProviderBindingRepository::list_for_model_provider(&conn, &provider_id)
            }
            (None, None) => Err(VibexError::validation(
                "agent_model_provider_binding_filter_missing",
                "binding list requires an Agent or model provider filter",
            )),
        }
    }

    pub fn create_agent_model_provider_binding(
        &self,
        request: AgentModelProviderBindingCreateRequest,
    ) -> VibexResult<AgentModelProviderBinding> {
        let conn = self.open_connection()?;
        let runtime = require_runtime(&conn, &request.runtime_profile_id)?;
        let provider = require_model_provider(&conn, &request.model_provider_profile_id)?;
        if request.agent_id != runtime.version_identity.route.agent_id {
            return Err(VibexError::validation(
                "agent_model_provider_binding_agent_mismatch",
                "binding Agent does not match the selected runtime profile",
            ));
        }
        let resolution =
            AgentProviderProjectionRegistry::builtin()?.resolve(&runtime.version_identity)?;
        if request.projection_descriptor_id != resolution.descriptor.id {
            return Err(VibexError::validation(
                "agent_projection_descriptor_mismatch",
                "binding descriptor does not match the compatible runtime identity",
            ));
        }
        let now = unix_timestamp_ms().max(1);
        let mut binding = AgentModelProviderBinding {
            id: AgentModelProviderBindingId::new(),
            legacy_provider_profile_id: None,
            agent_id: request.agent_id,
            runtime_profile_id: request.runtime_profile_id,
            model_provider_profile_id: request.model_provider_profile_id,
            projection_descriptor_id: request.projection_descriptor_id,
            projection_overrides: request.projection_overrides,
            configured_models: request.configured_models,
            projection_fingerprint: None,
            status: status_for_resolution(resolution.match_kind, &resolution.descriptor),
            verification: verification_from_descriptor(&resolution.descriptor),
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };
        let plan = AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            &resolution.descriptor,
            "binding-create",
        )?;
        if binding.status == AgentModelProviderBindingStatus::Ready {
            binding.projection_fingerprint = Some(plan.fingerprint);
        }
        AgentModelProviderBindingRepository::insert(&conn, &binding)?;
        Ok(binding)
    }

    pub fn update_agent_model_provider_binding(
        &self,
        request: AgentModelProviderBindingUpdateRequest,
    ) -> VibexResult<AgentModelProviderBinding> {
        let conn = self.open_connection()?;
        let existing = AgentModelProviderBindingRepository::get(&conn, &request.binding.id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_model_provider_binding_not_found",
                    "Agent model provider binding was not found",
                )
            })?;
        let runtime = require_runtime(&conn, &request.binding.runtime_profile_id)?;
        let provider = require_model_provider(&conn, &request.binding.model_provider_profile_id)?;
        let resolution =
            AgentProviderProjectionRegistry::builtin()?.resolve(&runtime.version_identity)?;
        if request.binding.projection_descriptor_id != resolution.descriptor.id {
            return Err(VibexError::validation(
                "agent_projection_descriptor_mismatch",
                "binding descriptor does not match the compatible runtime identity",
            ));
        }
        let mut binding = request.binding;
        binding.revision = request.expected_revision.saturating_add(1);
        binding.updated_at_ms = unix_timestamp_ms().max(existing.updated_at_ms.saturating_add(1));
        binding.projection_fingerprint = existing.projection_fingerprint.clone();
        binding.verification = verification_from_descriptor(&resolution.descriptor);
        let plan = AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            &resolution.descriptor,
            "binding-update",
        )?;
        apply_stale_state(&mut binding, &plan.fingerprint, resolution.match_kind);
        AgentModelProviderBindingRepository::update(&conn, &binding, request.expected_revision)
    }

    pub fn agent_provider_projection_capability(
        &self,
        request: AgentProviderProjectionCapabilityRequest,
    ) -> VibexResult<AgentProviderProjectionCapability> {
        let conn = self.open_connection()?;
        let runtime = require_runtime(&conn, &request.runtime_profile_id)?;
        let resolution =
            AgentProviderProjectionRegistry::builtin()?.resolve(&runtime.version_identity)?;
        let auth_state = request
            .binding_id
            .as_ref()
            .map(|binding_id| require_binding(&conn, binding_id))
            .transpose()?
            .map(|binding| require_model_provider(&conn, &binding.model_provider_profile_id))
            .transpose()?
            .as_ref()
            .map_or(ProjectionAuthState::Unknown, projection_auth_state);
        Ok(AgentProviderProjectionCapability::from_resolution(
            &runtime.version_identity,
            &resolution,
            auth_state,
        ))
    }

    pub fn preview_agent_provider_projection(
        &self,
        request: AgentProviderProjectionPreviewRequest,
    ) -> VibexResult<AgentProviderProjectionPreview> {
        Ok(self.plan_agent_provider_projection(&request)?.preview)
    }

    pub fn plan_agent_provider_projection(
        &self,
        request: &AgentProviderProjectionPreviewRequest,
    ) -> VibexResult<AgentProviderProjectionPlan> {
        let conn = self.open_connection()?;
        let (provider, runtime, binding, descriptor) =
            load_projection_input(&conn, &request.binding_id)?;
        AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            &descriptor,
            &request.workspace_key,
        )
    }

    pub fn resolve_agent_provider_projection(
        &self,
        request: &AgentProviderProjectionPreviewRequest,
    ) -> VibexResult<ResolvedAgentProviderProjection> {
        let plan = self.plan_agent_provider_projection(request)?;
        let runtime_root = self.projection_runtime_root()?;
        AgentProviderProjectionEngine::resolve_and_materialize(
            &plan,
            &runtime_root,
            &request.workspace_key,
        )
    }

    pub fn resolve_legacy_agent_provider_projection(
        &self,
        provider_profile_id: &vibex_core::ProviderProfileId,
        workspace_key: &str,
    ) -> VibexResult<ResolvedAgentProviderProjection> {
        let plan =
            self.plan_legacy_agent_provider_projection(provider_profile_id, workspace_key)?;
        let runtime_root = self.projection_runtime_root()?;
        AgentProviderProjectionEngine::resolve_and_materialize(&plan, &runtime_root, workspace_key)
    }

    /// Return the projection launch identity when a legacy profile is backed
    /// by an Agent/model-provider binding. ACP profiles that have not been
    /// migrated into that compatibility path deliberately return `None`.
    pub fn legacy_agent_provider_projection_runtime_plan(
        &self,
        provider_profile_id: &vibex_core::ProviderProfileId,
        workspace_key: &str,
    ) -> VibexResult<Option<LegacyAgentProviderProjectionRuntimePlan>> {
        let plan =
            match self.plan_legacy_agent_provider_projection(provider_profile_id, workspace_key) {
                Ok(plan) => plan,
                Err(error) if error.code == "agent_projection_legacy_binding_missing" => {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
        let runtime_root = self.projection_runtime_root()?;
        let non_secret_env = AgentProviderProjectionEngine::non_secret_environment_for_plan(
            &plan,
            &runtime_root,
            workspace_key,
        )?;
        let process_args = AgentProviderProjectionEngine::process_args_for_plan(
            &plan,
            &runtime_root,
            workspace_key,
        )?;
        Ok(Some(LegacyAgentProviderProjectionRuntimePlan {
            plan,
            non_secret_env,
            process_args,
        }))
    }

    /// Maps product-facing configured model ids to the exact ids understood by
    /// the selected Agent projection. OpenCode namespaces models by the
    /// generated provider id; other projections retain their declared Agent id.
    pub fn legacy_agent_provider_model_id_projections(
        &self,
        provider_profile_id: &vibex_core::ProviderProfileId,
    ) -> VibexResult<Option<Vec<LegacyAgentProviderModelIdProjection>>> {
        let conn = self.open_connection()?;
        let Some(binding) =
            AgentModelProviderBindingRepository::get_by_legacy_profile(&conn, provider_profile_id)?
        else {
            return Ok(None);
        };
        let (provider, _, binding, descriptor) = load_projection_input(&conn, &binding.id)?;
        Ok(Some(
            binding
                .configured_models
                .iter()
                .filter(|model| model.enabled)
                .map(|model| LegacyAgentProviderModelIdProjection {
                    product_model_id: model.provider_model_id.clone(),
                    runtime_model_id: projected_runtime_model_id(&provider, &descriptor, model),
                })
                .collect(),
        ))
    }

    pub fn plan_legacy_agent_provider_projection(
        &self,
        provider_profile_id: &vibex_core::ProviderProfileId,
        workspace_key: &str,
    ) -> VibexResult<AgentProviderProjectionPlan> {
        let conn = self.open_connection()?;
        let binding =
            AgentModelProviderBindingRepository::get_by_legacy_profile(&conn, provider_profile_id)?
                .ok_or_else(|| {
                    VibexError::validation(
                        "agent_projection_legacy_binding_missing",
                        "legacy provider profile has no projection binding",
                    )
                })?;
        drop(conn);
        self.plan_agent_provider_projection(&AgentProviderProjectionPreviewRequest {
            binding_id: binding.id,
            workspace_key: workspace_key.to_string(),
        })
    }

    pub fn mutate_provider_credential_secret(
        &self,
        request: ProviderCredentialSecretMutationRequest,
    ) -> VibexResult<ModelProviderProfile> {
        let conn = self.open_connection()?;
        let mut profile = require_model_provider(&conn, &request.model_provider_profile_id)?;
        if !request.touched {
            return Ok(profile);
        }
        let credential = profile
            .credentials
            .iter_mut()
            .find(|credential| credential.id == request.credential_id)
            .ok_or_else(|| {
                VibexError::validation(
                    "model_provider_credential_not_found",
                    "model provider credential was not found",
                )
            })?;
        let secret = credential
            .credential
            .secret_reference()
            .cloned()
            .ok_or_else(|| {
                VibexError::validation(
                    "model_provider_credential_secret_unsupported",
                    "selected credential does not use a host-managed Secret value",
                )
            })?;
        let next_value = request
            .value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if !request.clear && next_value.is_none() {
            return Err(VibexError::validation(
                "model_provider_credential_secret_empty",
                "an explicitly touched Secret must contain a value or request clear",
            ));
        }

        let mut next_secret = secret.clone();
        if request.clear {
            if secret.backend == ProviderSecretBackend::OsKeychain {
                secrets::delete_provider_secret(&secret.lookup_key)?;
            }
            next_secret.backend = ProviderSecretBackend::Placeholder;
            next_secret.setup_state = ProviderSecretSetupState::Missing;
            next_secret.redacted_hint = "not configured".to_string();
        } else if let Some(value) = next_value {
            let lookup_key = if secret.backend == ProviderSecretBackend::OsKeychain
                && !secret.lookup_key.trim().is_empty()
            {
                secret.lookup_key.clone()
            } else {
                format!("vibex-provider-secret-{}", RequestId::new().as_str())
            };
            secrets::store_provider_secret(&lookup_key, value)?;
            next_secret.backend = ProviderSecretBackend::OsKeychain;
            next_secret.setup_state = ProviderSecretSetupState::Available;
            next_secret.lookup_key = lookup_key;
            next_secret.redacted_hint = "stored in Vibex OS keychain".to_string();
        }
        next_secret.revision = next_secret.revision.saturating_add(1).max(1);
        replace_credential_secret(&mut credential.credential, next_secret)?;
        credential.status = if request.clear {
            AgentCredentialStatus::Missing
        } else {
            AgentCredentialStatus::Ready
        };
        credential.revision = credential.revision.saturating_add(1).max(1);
        let expected_revision = profile.revision;
        profile.revision = profile.revision.saturating_add(1);
        profile.updated_at_ms = unix_timestamp_ms().max(profile.updated_at_ms.saturating_add(1));
        let updated = ModelProviderProfileRepository::update(&conn, &profile, expected_revision)?;
        self.refresh_bindings_for_model_provider(&conn, &updated.id)?;
        Ok(updated)
    }

    pub(crate) fn sync_legacy_projection(
        &self,
        conn: &vibex_db::DbConnection,
        legacy: &vibex_core::ProviderProfile,
    ) -> VibexResult<bool> {
        let previous_model_provider =
            ModelProviderProfileRepository::get_by_legacy_profile(conn, &legacy.id)?;
        let previous_agent_runtime =
            AgentRuntimeProfileRepository::get_by_legacy_profile(conn, &legacy.id)?;
        let previous_binding =
            AgentModelProviderBindingRepository::get_by_legacy_profile(conn, &legacy.id)?;
        let records = ProviderProjectionCompatibilityRepository::sync_legacy_profile(conn, legacy)?;
        let projection_rows_changed = previous_model_provider.as_ref()
            != Some(&records.model_provider)
            || previous_agent_runtime.as_ref() != Some(&records.agent_runtime)
            || previous_binding.as_ref() != Some(&records.binding);
        let projection_state_changed = self.refresh_one_binding(conn, records.binding)?;
        Ok(projection_rows_changed || projection_state_changed)
    }

    pub(crate) fn mark_legacy_projection_deleted(
        &self,
        conn: &vibex_db::DbConnection,
        legacy_id: &vibex_core::ProviderProfileId,
        deleted_at_ms: i64,
    ) -> VibexResult<()> {
        ProviderProjectionCompatibilityRepository::mark_legacy_deleted(
            conn,
            legacy_id,
            deleted_at_ms,
        )
    }

    fn projection_runtime_root(&self) -> VibexResult<PathBuf> {
        self.database_path()
            .parent()
            .map(|parent| parent.join("runtime").join(PROJECTION_RUNTIME_DIR))
            .ok_or_else(|| {
                VibexError::storage(
                    "agent_projection_runtime_parent_missing",
                    "projection database path has no parent directory",
                )
            })
    }

    fn refresh_bindings_for_model_provider(
        &self,
        conn: &vibex_db::DbConnection,
        provider_id: &ModelProviderProfileId,
    ) -> VibexResult<()> {
        for binding in
            AgentModelProviderBindingRepository::list_for_model_provider(conn, provider_id)?
        {
            self.refresh_one_binding(conn, binding)?;
        }
        Ok(())
    }

    fn refresh_bindings_for_runtime(
        &self,
        conn: &vibex_db::DbConnection,
        runtime_id: &AgentRuntimeProfileId,
    ) -> VibexResult<()> {
        for binding in AgentModelProviderBindingRepository::list_for_runtime(conn, runtime_id)? {
            self.refresh_one_binding(conn, binding)?;
        }
        Ok(())
    }

    fn refresh_one_binding(
        &self,
        conn: &vibex_db::DbConnection,
        mut binding: AgentModelProviderBinding,
    ) -> VibexResult<bool> {
        let provider = require_model_provider(conn, &binding.model_provider_profile_id)?;
        let runtime = require_runtime(conn, &binding.runtime_profile_id)?;
        let resolution =
            AgentProviderProjectionRegistry::builtin()?.resolve(&runtime.version_identity)?;
        if binding.projection_descriptor_id != resolution.descriptor.id {
            binding.status = AgentModelProviderBindingStatus::Unsupported;
            binding.verification.state = ProjectionEvidenceState::Stale;
            return persist_projection_state_if_changed(conn, &binding, None);
        }
        let plan = AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            &resolution.descriptor,
            "stale-check",
        )?;
        let active_fingerprint = binding.projection_fingerprint.clone();
        binding.verification = verification_from_descriptor(&resolution.descriptor);
        apply_stale_state(&mut binding, &plan.fingerprint, resolution.match_kind);
        persist_projection_state_if_changed(conn, &binding, active_fingerprint.as_deref())
    }
}

fn selected_endpoint<'a>(
    provider: &'a ModelProviderProfile,
    binding: &AgentModelProviderBinding,
    model: Option<&AgentConfiguredModelBinding>,
) -> VibexResult<Option<&'a ModelProviderEndpoint>> {
    if let Some(endpoint_id) = binding.projection_overrides.endpoint_id.as_deref() {
        let endpoint = provider
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == endpoint_id)
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_projection_endpoint_not_found",
                    "binding endpoint override was not found in the model provider profile",
                )
            })?;
        if let (Some(endpoint_protocol), Some(model)) =
            (endpoint.wire_protocol_id.as_deref(), model)
            && endpoint_protocol != model.wire_protocol_id
        {
            return Err(VibexError::validation(
                "agent_projection_endpoint_protocol_mismatch",
                "binding endpoint does not support the selected model wire protocol",
            )
            .with_diagnostic("endpointId", endpoint.id.as_str())
            .with_diagnostic("wireProtocolId", model.wire_protocol_id.as_str()));
        }
        return Ok(Some(endpoint));
    }
    Ok(model.map_or_else(
        || provider.primary_api_endpoint(),
        |model| provider.primary_api_endpoint_for_protocol(&model.wire_protocol_id),
    ))
}

fn selected_credential<'a>(
    provider: &'a ModelProviderProfile,
    binding: &AgentModelProviderBinding,
) -> VibexResult<Option<&'a ModelProviderCredentialReference>> {
    if let Some(credential_id) = binding.projection_overrides.credential_id.as_ref() {
        return provider
            .credentials
            .iter()
            .find(|credential| &credential.id == credential_id)
            .map(Some)
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_projection_credential_not_found",
                    "binding credential override was not found in the model provider profile",
                )
            });
    }
    Ok(provider.credentials.first())
}

fn selected_model<'a>(
    provider: &ModelProviderProfile,
    binding: &'a AgentModelProviderBinding,
) -> VibexResult<Option<&'a AgentConfiguredModelBinding>> {
    if let Some(binding_id) = binding
        .projection_overrides
        .default_model_binding_id
        .as_ref()
    {
        return binding
            .configured_models
            .iter()
            .find(|model| &model.id == binding_id && model.enabled)
            .map(Some)
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_projection_default_model_binding_invalid",
                    "default model binding override must reference an enabled model binding",
                )
            });
    }
    Ok(provider
        .default_model_id
        .as_deref()
        .and_then(|model_id| {
            binding
                .configured_models
                .iter()
                .find(|model| model.provider_model_id == model_id && model.enabled)
        })
        .or_else(|| binding.configured_models.iter().find(|model| model.enabled)))
}

#[allow(clippy::too_many_arguments)]
fn project_provider_control(
    descriptor: &AgentProviderProjectionDescriptor,
    provider: &ModelProviderProfile,
    binding: &AgentModelProviderBinding,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    env: &mut BTreeMap<String, String>,
    overlays: &mut Vec<ManagedProjectionOverlay>,
    session: &mut Vec<ProviderBindingMetadata>,
    targets: &mut Vec<ProjectionTargetPreview>,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
) -> VibexResult<()> {
    match &descriptor.provider_control {
        AgentProviderControl::Environment { base_url_key } => {
            if let (Some(key), Some(endpoint)) = (base_url_key, endpoint) {
                let endpoint_url = environment_base_url(descriptor, &endpoint.url);
                env.insert(key.clone(), endpoint_url.clone());
                targets.push(endpoint_target(key, &endpoint_url));
            }
        }
        AgentProviderControl::ManagedConfigOverlay { strategy } => {
            let overlay = build_overlay(
                strategy,
                provider,
                binding,
                endpoint,
                model,
                descriptor_secret_env_key(descriptor),
            )?;
            if *strategy == ConfigOverlayStrategy::OpenCodeInlineProvider {
                env.insert(OPENCODE_CONFIG_ENV.to_string(), overlay.content.clone());
            } else if *strategy == ConfigOverlayStrategy::CodexStableHome {
                env.insert(
                    CODEX_MODEL_PROVIDER_ENV.to_string(),
                    sanitize_provider_id(
                        provider
                            .vendor_hint
                            .as_deref()
                            .unwrap_or_else(|| provider.id.as_str()),
                    ),
                );
                env.insert(
                    CODEX_DEFAULT_AUTH_REQUEST_ENV.to_string(),
                    CODEX_DEFAULT_API_KEY_AUTH_REQUEST.to_string(),
                );
            } else if let Some(key) = inline_overlay_env_key(strategy) {
                env.insert(key.to_string(), overlay.content.clone());
            }
            targets.push(ProjectionTargetPreview {
                field: "endpoint".to_string(),
                target_kind: ProjectionTargetKind::ManagedOverlay,
                target: overlay.relative_path.clone(),
                value_preview: endpoint
                    .map(|endpoint| redact_endpoint(&endpoint.url))
                    .unwrap_or_else(|| "not configured".to_string()),
                secret: false,
            });
            overlays.push(overlay);
            if *strategy == ConfigOverlayStrategy::KimiToml {
                overlays.push(kimi_auth_compatibility_overlay(require_secret_env_key(
                    descriptor_secret_env_key(descriptor),
                )?));
            }
        }
        AgentProviderControl::AdvertisedSessionOption { option_ids } => {
            session.push(ProviderBindingMetadata {
                key: "providerOptionIds".to_string(),
                value: option_ids.join(","),
            });
        }
        AgentProviderControl::AgentManaged => targets.push(state_target(
            ProjectionTargetKind::AgentManaged,
            "Agent account settings",
        )),
        AgentProviderControl::LocalModel => targets.push(state_target(
            ProjectionTargetKind::LocalRuntime,
            "local model runtime",
        )),
        AgentProviderControl::ServiceMarketplace => targets.push(state_target(
            ProjectionTargetKind::ServiceMarketplace,
            "service marketplace",
        )),
        AgentProviderControl::Unsupported | AgentProviderControl::Unverified => {
            diagnostics.push(ProviderBindingMetadata {
                key: "providerProjection".to_string(),
                value: "automatic projection disabled".to_string(),
            });
            targets.push(state_target(
                ProjectionTargetKind::None,
                "no automatic target",
            ));
        }
    }
    Ok(())
}

fn project_credential_control(
    descriptor: &AgentProviderProjectionDescriptor,
    credential: Option<&ModelProviderCredentialReference>,
    secret_env: &mut Vec<ProjectionSecretEnvReference>,
    targets: &mut Vec<ProjectionTargetPreview>,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
) -> VibexResult<()> {
    match &descriptor.credential_control {
        AgentCredentialControl::Environment { secret_env_key, .. } => {
            let Some(credential) = credential else {
                diagnostics.push(ProviderBindingMetadata {
                    key: "credential".to_string(),
                    value: "missing".to_string(),
                });
                return Ok(());
            };
            let Some(secret_reference) = credential.credential.secret_reference() else {
                return Err(VibexError::validation(
                    "agent_projection_secret_reference_missing",
                    "selected credential cannot be projected to the required Secret environment target",
                ));
            };
            let secret_env_key = credential_secret_env_key(descriptor, credential, secret_env_key);
            secret_env.push(ProjectionSecretEnvReference {
                key: secret_env_key.to_string(),
                credential_id: credential.id.clone(),
                secret_reference: secret_reference.clone(),
            });
            targets.push(ProjectionTargetPreview {
                field: "credential".to_string(),
                target_kind: ProjectionTargetKind::Environment,
                target: secret_env_key.to_string(),
                value_preview: if secret_reference.setup_state
                    == ProviderSecretSetupState::Available
                {
                    "configured".to_string()
                } else {
                    "missing".to_string()
                },
                secret: true,
            });
        }
        AgentCredentialControl::ManagedConfigOverlay { .. } => {
            if credential
                .and_then(|value| value.credential.secret_reference())
                .is_some()
            {
                return Err(VibexError::capability(
                    "agent_projection_overlay_secret_strategy_unavailable",
                    "this descriptor version does not have a code-owned Secret overlay strategy",
                ));
            }
        }
        AgentCredentialControl::AdvertisedAuthMethod { method_ids } => {
            targets.push(ProjectionTargetPreview {
                field: "credential".to_string(),
                target_kind: ProjectionTargetKind::AdvertisedAuthMethod,
                target: method_ids.join(","),
                value_preview: "Agent advertised auth".to_string(),
                secret: false,
            });
        }
        AgentCredentialControl::OAuthAgentManaged | AgentCredentialControl::AgentManaged => {
            targets.push(state_target(
                ProjectionTargetKind::AgentManaged,
                "Agent login state",
            ));
        }
        AgentCredentialControl::Local => targets.push(state_target(
            ProjectionTargetKind::LocalRuntime,
            "local credential state",
        )),
        AgentCredentialControl::ServiceMarketplace => targets.push(state_target(
            ProjectionTargetKind::ServiceMarketplace,
            "marketplace credential state",
        )),
        AgentCredentialControl::Unsupported | AgentCredentialControl::Unverified => {}
    }
    Ok(())
}

fn project_model_control(
    descriptor: &AgentProviderProjectionDescriptor,
    model: Option<&AgentConfiguredModelBinding>,
    env: &mut BTreeMap<String, String>,
    session: &mut Vec<ProviderBindingMetadata>,
    targets: &mut Vec<ProjectionTargetPreview>,
) {
    let Some(model) = model else {
        return;
    };
    match &descriptor.model_control {
        AgentModelControl::AcpSetModel => {
            session.push(metadata("model", &model.agent_model_id));
            targets.push(model_target(
                ProjectionTargetKind::AcpModel,
                "session/set_model",
                model,
            ));
        }
        AgentModelControl::AcpConfigOption { aliases } => {
            session.push(metadata("model", &model.agent_model_id));
            targets.push(model_target(
                ProjectionTargetKind::AcpConfigOption,
                &aliases.join(","),
                model,
            ));
        }
        AgentModelControl::ProcessEnvironment { key } => {
            env.insert(key.clone(), model.agent_model_id.clone());
            targets.push(model_target(ProjectionTargetKind::Environment, key, model));
        }
        AgentModelControl::ManagedConfigOverlay { .. } => targets.push(model_target(
            ProjectionTargetKind::ManagedOverlay,
            "managed provider overlay",
            model,
        )),
        AgentModelControl::AgentManaged => targets.push(model_target(
            ProjectionTargetKind::AgentManaged,
            "Agent model settings",
            model,
        )),
        AgentModelControl::LocalModel => targets.push(model_target(
            ProjectionTargetKind::LocalRuntime,
            "local model runtime",
            model,
        )),
        AgentModelControl::ServiceMarketplace => targets.push(model_target(
            ProjectionTargetKind::ServiceMarketplace,
            "service marketplace",
            model,
        )),
        AgentModelControl::Unsupported | AgentModelControl::Unverified => {}
    }
}

/// Gemini CLI and Antigravity treat `GOOGLE_GEMINI_BASE_URL` as an origin and
/// append the Google Generative Language API path themselves. Provider forms
/// commonly store an OpenAI-style `/v1` suffix; forwarding it verbatim produces
/// `/v1/v1beta/models/...` and makes every prompt fail after a successful ACP
/// handshake. Keep other Agents byte-for-byte unchanged.
fn environment_base_url(
    descriptor: &AgentProviderProjectionDescriptor,
    endpoint_url: &str,
) -> String {
    if !matches!(descriptor.route.agent_id.as_str(), "antigravity" | "gemini") {
        return endpoint_url.to_string();
    }

    let trimmed = endpoint_url.trim_end_matches('/');
    for api_suffix in ["/v1beta", "/v1"] {
        if let Some(origin) = trimmed.strip_suffix(api_suffix)
            && !origin.is_empty()
        {
            return origin.to_string();
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod google_generative_ai_base_url_tests {
    use super::*;

    #[test]
    fn google_generative_ai_environment_base_url_removes_api_path_suffixes() {
        let registry = AgentProviderProjectionRegistry::builtin().unwrap();

        for agent_id in ["antigravity", "gemini"] {
            let descriptor = registry
                .descriptors()
                .find(|descriptor| descriptor.route.agent_id.as_str() == agent_id)
                .cloned()
                .unwrap();

            assert_eq!(
                environment_base_url(&descriptor, "https://gateway.example/v1"),
                "https://gateway.example"
            );
            assert_eq!(
                environment_base_url(&descriptor, "https://gateway.example/v1beta/"),
                "https://gateway.example"
            );
            assert_eq!(
                environment_base_url(&descriptor, "https://gateway.example/custom/"),
                "https://gateway.example/custom"
            );
        }
    }
}

fn descriptor_secret_env_key(descriptor: &AgentProviderProjectionDescriptor) -> Option<&str> {
    match &descriptor.credential_control {
        AgentCredentialControl::Environment { secret_env_key, .. } => Some(secret_env_key),
        _ => None,
    }
}

fn credential_secret_env_key<'a>(
    descriptor: &AgentProviderProjectionDescriptor,
    credential: &ModelProviderCredentialReference,
    fallback: &'a str,
) -> &'a str {
    if descriptor.route.agent_id.as_str() != CLAUDE_AGENT_ID {
        return fallback;
    }
    match credential.display_name.trim() {
        CLAUDE_AUTH_TOKEN_ENV => CLAUDE_AUTH_TOKEN_ENV,
        CLAUDE_API_KEY_ENV => CLAUDE_API_KEY_ENV,
        _ => fallback,
    }
}

fn inline_overlay_env_key(strategy: &ConfigOverlayStrategy) -> Option<&'static str> {
    match strategy {
        ConfigOverlayStrategy::KiloInlineJson => Some("KILO_CONFIG_CONTENT"),
        _ => None,
    }
}

fn private_home_env_key(agent_id: &str) -> Option<&'static str> {
    match agent_id {
        "antigravity" => Some("GEMINI_HOME"),
        "copilot" => Some("COPILOT_HOME"),
        "codewhale" => Some("CODEWHALE_HOME"),
        "codex" => Some("CODEX_HOME"),
        "crow-cli" => None,
        "dirac" => Some("DIRAC_DIR"),
        "factory-droid" => Some("FACTORY_HOME_OVERRIDE"),
        "gemini" => Some("GEMINI_HOME"),
        "goose" => Some("GOOSE_PATH_ROOT"),
        "grok" => Some("GROK_HOME"),
        "hermes" => Some("HERMES_HOME"),
        "kilo" => None,
        "kimi" => Some("KIMI_SHARE_DIR"),
        "mistral-vibe" => Some("VIBE_HOME"),
        "pi" => Some("PI_CODING_AGENT_DIR"),
        "qwen-code" => Some("QWEN_HOME"),
        "stakpak" => None,
        "vtcode" => Some("VTCODE_CONFIG_PATH"),
        // zcode-acp-server resolves provider state from
        // $HOME/.zcode/v2/config.json. Point HOME at the stable private
        // projection root so Provider Profiles never rewrite native ZCode
        // configuration in the user's real home.
        "zcode" => Some("HOME"),
        _ => None,
    }
}

fn runtime_home_env_value(
    key: &str,
    overlay_root: &Path,
    overlays: &[ManagedProjectionOverlay],
) -> String {
    if key == "VTCODE_CONFIG_PATH"
        && overlays
            .iter()
            .any(|overlay| overlay.relative_path == "vtcode.toml")
    {
        return overlay_root
            .join("vtcode.toml")
            .to_string_lossy()
            .into_owned();
    }
    overlay_root.to_string_lossy().into_owned()
}

fn process_args_for_overlay_root(args: &[String], overlay_root: &Path) -> Vec<String> {
    let root = overlay_root.to_string_lossy();
    args.iter()
        .map(|argument| argument.replace("{projectionRoot}", &root))
        .collect()
}

fn projection_process_arg_templates(agent_id: &str) -> Vec<String> {
    match agent_id {
        "crow-cli" => vec!["--config-dir".to_string(), "{projectionRoot}".to_string()],
        "stakpak" => vec![
            "--profile".to_string(),
            "vibex".to_string(),
            "--config".to_string(),
            "{projectionRoot}/stakpak.toml".to_string(),
        ],
        _ => Vec::new(),
    }
}

fn apply_agent_projection_defaults(
    descriptor: &AgentProviderProjectionDescriptor,
    provider: &ModelProviderProfile,
    model: Option<&AgentConfiguredModelBinding>,
    endpoint: Option<&ModelProviderEndpoint>,
    env: &mut BTreeMap<String, String>,
) {
    let agent_id = descriptor.route.agent_id.as_str();
    match agent_id {
        "codewhale" => {
            env.insert("CODEWHALE_PROVIDER".to_string(), "openai".to_string());
        }
        "goose" => {
            env.insert("GOOSE_PROVIDER".to_string(), goose_provider_id(provider));
            env.insert(
                "GOOSE_MODEL".to_string(),
                projection_model_id(model)
                    .unwrap_or("vibex-model")
                    .to_string(),
            );
        }
        "poolside" => {
            if let Some(endpoint) = endpoint {
                env.insert(
                    "POOLSIDE_STANDALONE_BASE_URL".to_string(),
                    normalize_poolside_base_url(&endpoint.url),
                );
            }
        }
        _ => {}
    }
}

fn normalize_poolside_base_url(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if let Ok(mut url) = reqwest::Url::parse(trimmed) {
        let path = url.path().trim_end_matches('/');
        if path == "/v1" {
            url.set_path("");
        }
        return url.to_string().trim_end_matches('/').to_string();
    }
    trimmed
        .strip_suffix("/v1")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

fn build_overlay(
    strategy: &ConfigOverlayStrategy,
    provider: &ModelProviderProfile,
    binding: &AgentModelProviderBinding,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: Option<&str>,
) -> VibexResult<ManagedProjectionOverlay> {
    let (relative_path, format, content, contains_secret_reference) = match strategy {
        ConfigOverlayStrategy::CodexStableHome => (
            "config.toml",
            "toml",
            codex_overlay(provider, endpoint, model),
            false,
        ),
        ConfigOverlayStrategy::OpenCodeInlineProvider => (
            "opencode.json",
            "json",
            opencode_overlay(provider, binding, endpoint)?,
            provider
                .credentials
                .iter()
                .any(|credential| credential.credential.secret_reference().is_some()),
        ),
        ConfigOverlayStrategy::StructuredJsonOverlay => (
            "provider.json",
            "json",
            structured_json_overlay(provider, endpoint, model)?,
            false,
        ),
        ConfigOverlayStrategy::StructuredTomlOverlay => (
            "provider.toml",
            "toml",
            structured_toml_overlay(provider, endpoint, model)?,
            false,
        ),
        ConfigOverlayStrategy::StructuredYamlOverlay => (
            "provider.yaml",
            "yaml",
            structured_yaml_overlay(provider, endpoint, model)?,
            false,
        ),
        ConfigOverlayStrategy::CrowCliYaml => (
            "config.yaml",
            "yaml",
            crow_cli_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::DiracToml => (
            "data/globalState.json",
            "json",
            dirac_overlay(provider, endpoint, model)?,
            false,
        ),
        ConfigOverlayStrategy::FactoryDroidJson => (
            "settings.json",
            "json",
            factory_droid_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::GooseJson => (
            "config/custom_providers/vibex.json",
            "json",
            goose_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::GrokToml => (
            "config.toml",
            "toml",
            grok_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::HermesYaml => (
            "config.yaml",
            "yaml",
            hermes_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::KiloInlineJson => (
            "kilo.json",
            "json",
            kilo_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::KimiToml => (
            "config.toml",
            "toml",
            kimi_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::MistralVibeToml => (
            "config.toml",
            "toml",
            mistral_vibe_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::PiModelsJson => (
            "models.json",
            "json",
            pi_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::QwenCodeJson => (
            "settings.json",
            "json",
            qwen_code_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::StakpakToml => (
            "stakpak.toml",
            "toml",
            stakpak_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            false,
        ),
        ConfigOverlayStrategy::VtcodeToml => (
            "vtcode.toml",
            "toml",
            vtcode_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::ZcodeJson => (
            ".zcode/v2/config.json",
            "json",
            zcode_overlay(
                provider,
                endpoint,
                model,
                require_secret_env_key(secret_env_key)?,
            )?,
            true,
        ),
        ConfigOverlayStrategy::ClaudeEnvironment
        | ConfigOverlayStrategy::GenericEnvironmentDescriptor => {
            return Err(VibexError::validation(
                "agent_projection_overlay_strategy_invalid",
                "environment projection strategy cannot be used as a managed overlay",
            ));
        }
    };
    Ok(ManagedProjectionOverlay {
        relative_path: relative_path.to_string(),
        format: format.to_string(),
        content,
        contains_secret_reference,
    })
}

fn codex_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
) -> String {
    let provider_id = sanitize_provider_id(
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str()),
    );
    let mut lines = vec![format!("model_provider = {}", toml_quote(&provider_id))];
    if let Some(model) = model {
        // Select Provider-configured models before codex-acp creates the
        // session. codex-acp's model/list is its built-in catalogue and its
        // live set-model path rejects otherwise valid custom-provider ids.
        lines.push(format!("model = {}", toml_quote(&model.agent_model_id)));
    }
    lines.push(String::new());
    lines.push(format!("[model_providers.{}]", toml_quote(&provider_id)));
    lines.push(format!("name = {}", toml_quote(&provider.display_name)));
    if let Some(endpoint) = endpoint {
        lines.push(format!("base_url = {}", toml_quote(&endpoint.url)));
    }
    lines.push("wire_api = \"responses\"".to_string());
    lines.push("requires_openai_auth = true".to_string());
    lines.push("env_key = \"CODEX_API_KEY\"".to_string());
    // Codex app-server derives its default originator from the ACP client name.
    // Keep the ACP identity truthful while identifying the actual HTTP engine to
    // Codex-compatible gateways that route or authorize by request originator.
    lines.push(format!(
        "http_headers = {{ originator = {} }}",
        toml_quote(CODEX_PROVIDER_ORIGINATOR)
    ));
    format!("{}\n", lines.join("\n"))
}

fn opencode_overlay(
    provider: &ModelProviderProfile,
    binding: &AgentModelProviderBinding,
    endpoint: Option<&ModelProviderEndpoint>,
) -> VibexResult<String> {
    let base_provider_id = sanitize_provider_id(
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str()),
    );
    let mut providers = BTreeMap::<String, serde_json::Value>::new();
    for model in binding
        .configured_models
        .iter()
        .filter(|model| model.enabled)
    {
        let (provider_id, npm) = opencode_provider_identity(&base_provider_id, model);
        let model_endpoint = opencode_endpoint_for_model(provider, binding, endpoint, model);
        let model_endpoint_url = model_endpoint
            .map(|endpoint| opencode_base_url_for_model(&endpoint.url, &model.wire_protocol_id));
        let entry = providers.entry(provider_id.clone()).or_insert_with(|| {
            let mut options = serde_json::Map::new();
            if let Some(endpoint) = model_endpoint_url.as_deref() {
                options.insert(
                    "baseURL".to_string(),
                    serde_json::Value::String(endpoint.to_string()),
                );
            }
            serde_json::json!({
                "name": provider.display_name,
                "npm": npm,
                "options": options,
                "models": {}
            })
        });
        if let Some(models) = entry
            .get_mut("models")
            .and_then(serde_json::Value::as_object_mut)
        {
            let catalog_entry = provider
                .configured_models
                .iter()
                .find(|entry| entry.id == model.provider_model_id);
            let mut model_config = serde_json::Map::new();
            if let Some(display_name) = catalog_entry.and_then(|entry| entry.display_name.clone()) {
                model_config.insert("name".to_string(), serde_json::Value::String(display_name));
            }
            opencode_apply_model_capabilities(
                &mut model_config,
                catalog_entry.map(|entry| &entry.capabilities),
            );
            models.insert(
                opencode_model_key(&provider_id, &model.agent_model_id),
                serde_json::Value::Object(model_config),
            );
        }
    }
    if providers.is_empty() {
        providers.insert(
            base_provider_id.clone(),
            serde_json::json!({
                "name": provider.display_name,
                "npm": "@ai-sdk/openai",
                "options": endpoint.map_or_else(serde_json::Map::new, |endpoint| {
                    serde_json::Map::from_iter([(
                        "baseURL".to_string(),
                        serde_json::Value::String(endpoint.url.clone()),
                    )])
                }),
                "models": {}
            }),
        );
    }
    for value in providers.values_mut() {
        let npm = value
            .get("npm")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(options) = value
            .get_mut("options")
            .and_then(serde_json::Value::as_object_mut)
        {
            if provider
                .credentials
                .iter()
                .any(|credential| credential.credential.secret_reference().is_some())
            {
                let secret_reference = format!("{{env:{OPENCODE_SECRET_ENV}}}");
                if npm == "@ai-sdk/amazon-bedrock" {
                    let headers = options
                        .entry("headers".to_string())
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(headers) = headers.as_object_mut() {
                        headers.insert(
                            "Authorization".to_string(),
                            serde_json::Value::String(format!("Bearer {secret_reference}")),
                        );
                    }
                } else {
                    options.insert(
                        "apiKey".to_string(),
                        serde_json::Value::String(secret_reference),
                    );
                }
            }
            for header in &provider.headers {
                if let ModelProviderHeaderValue::NonSecretLiteral(value) = &header.value {
                    let headers = options
                        .entry("headers".to_string())
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(headers) = headers.as_object_mut() {
                        headers.insert(
                            header.name.clone(),
                            serde_json::Value::String(value.clone()),
                        );
                    }
                }
            }
        }
    }
    let enabled_providers = providers.keys().cloned().collect::<Vec<_>>();
    let mut root = BTreeMap::<String, serde_json::Value>::new();
    root.insert(
        "$schema".to_string(),
        serde_json::Value::String("https://opencode.ai/config.json".to_string()),
    );
    root.insert(
        "provider".to_string(),
        serde_json::to_value(providers).map_err(encode_error)?,
    );
    root.insert(
        "enabled_providers".to_string(),
        serde_json::to_value(enabled_providers).map_err(encode_error)?,
    );
    if let Some(default_model) = provider.default_model_id.as_deref() {
        let qualified = binding
            .configured_models
            .iter()
            .find(|model| model.enabled && model.provider_model_id == default_model)
            .map(|model| {
                let (provider_id, _) = opencode_provider_identity(&base_provider_id, model);
                format!(
                    "{provider_id}/{}",
                    opencode_model_key(&provider_id, &model.agent_model_id)
                )
            })
            .unwrap_or_else(|| format!("{base_provider_id}/{default_model}"));
        root.insert("model".to_string(), serde_json::Value::String(qualified));
    }
    serde_json::to_string(&root).map_err(encode_error)
}

fn opencode_endpoint_for_model<'a>(
    provider: &'a ModelProviderProfile,
    binding: &AgentModelProviderBinding,
    selected_endpoint: Option<&'a ModelProviderEndpoint>,
    model: &AgentConfiguredModelBinding,
) -> Option<&'a ModelProviderEndpoint> {
    if binding.projection_overrides.endpoint_id.is_some()
        && let Some(endpoint) = selected_endpoint
        && endpoint
            .wire_protocol_id
            .as_deref()
            .is_none_or(|protocol| protocol == model.wire_protocol_id)
    {
        return Some(endpoint);
    }
    provider.primary_api_endpoint_for_protocol(&model.wire_protocol_id)
}

/// OpenCode delegates Anthropic requests to the AI SDK, whose `baseURL` is the
/// API root and whose transport appends `/messages`. Vibex provider endpoints
/// are stored as provider roots, so add the Anthropic `/v1` prefix at this
/// boundary while preserving already-versioned or full `/messages` URLs.
fn opencode_base_url_for_model(endpoint: &str, wire_protocol_id: &str) -> String {
    if wire_protocol_id != vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES {
        return endpoint.to_string();
    }
    let trimmed = endpoint.trim().trim_end_matches('/');

    let Ok(mut url) = reqwest::Url::parse(trimmed) else {
        return if trimmed.ends_with("/v1") || trimmed.ends_with("/messages") {
            trimmed.to_string()
        } else {
            format!("{trimmed}/v1")
        };
    };
    let path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/messages") {
        url.set_path(path.trim_end_matches("/messages"));
    } else if !path.ends_with("/v1") {
        let versioned_path = if path.is_empty() {
            "/v1".to_string()
        } else {
            format!("{path}/v1")
        };
        url.set_path(&versioned_path);
    }
    url.to_string().trim_end_matches('/').to_string()
}

fn opencode_provider_identity<'a>(
    base_provider_id: &str,
    model: &'a AgentConfiguredModelBinding,
) -> (String, &'a str) {
    let npm = model.sdk_adapter_id.as_deref().unwrap_or("@ai-sdk/openai");
    let suffix = match model.wire_protocol_id.as_str() {
        vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES => "responses",
        vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS => "chat",
        vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => "anthropic",
        vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI => "google",
        vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE => "bedrock",
        _ => "custom",
    };
    let provider_id = if suffix == "responses" {
        base_provider_id.to_string()
    } else {
        format!("{base_provider_id}-{suffix}")
    };
    (provider_id, npm)
}

/// Project declared Model capabilities into an OpenCode model object.
///
/// OpenCode resolves every capability it is not told about to `false`, because
/// a Vibex-generated provider id never matches a models.dev provider and so
/// inherits no upstream metadata. An omitted flag therefore reads as
/// "unsupported" and silently disables reasoning depth, run modes, and image
/// input for the Model.
///
/// Only explicitly declared capabilities are emitted. An undeclared field stays
/// absent so a Model with nothing declared still projects as `{}`, which keeps
/// the documented empty-model-object contract intact. Vibex never infers a
/// capability from a Model id.
///
/// OpenCode derives its own run-mode variants from `reasoning` plus the Model
/// id, so Vibex declares the capability and leaves the effort levels to
/// OpenCode.
fn opencode_apply_model_capabilities(
    model_config: &mut serde_json::Map<String, serde_json::Value>,
    capabilities: Option<&ProviderModelCapabilities>,
) {
    let Some(capabilities) = capabilities.filter(|capabilities| !capabilities.is_empty()) else {
        return;
    };
    if let Some(reasoning) = capabilities.reasoning {
        model_config.insert("reasoning".to_string(), serde_json::Value::Bool(reasoning));
    }
    if let Some(temperature) = capabilities.temperature {
        model_config.insert(
            "temperature".to_string(),
            serde_json::Value::Bool(temperature),
        );
    }

    // `attachment` gates prompt attachments as a whole; `modalities.input`
    // gates the individual part types. OpenCode needs both to accept an image.
    let image_input = capabilities.image_input.unwrap_or(false);
    let pdf_input = capabilities.pdf_input.unwrap_or(false);
    if capabilities.image_input.is_some() || capabilities.pdf_input.is_some() {
        model_config.insert(
            "attachment".to_string(),
            serde_json::Value::Bool(image_input || pdf_input),
        );
        let mut input = vec![serde_json::Value::String("text".to_string())];
        if image_input {
            input.push(serde_json::Value::String("image".to_string()));
        }
        if pdf_input {
            input.push(serde_json::Value::String("pdf".to_string()));
        }
        model_config.insert(
            "modalities".to_string(),
            serde_json::json!({
                "input": input,
                "output": ["text"],
            }),
        );
    }

    // OpenCode requires both bounds together, and treats zero as "unknown".
    if let (Some(context), Some(output)) = (capabilities.context_tokens, capabilities.output_tokens)
    {
        model_config.insert(
            "limit".to_string(),
            serde_json::json!({ "context": context, "output": output }),
        );
    }
}

fn opencode_model_key(provider_id: &str, model_id: &str) -> String {
    model_id
        .strip_prefix(&format!("{provider_id}/"))
        .unwrap_or(model_id)
        .to_string()
}

fn projected_runtime_model_id(
    provider: &ModelProviderProfile,
    descriptor: &AgentProviderProjectionDescriptor,
    model: &AgentConfiguredModelBinding,
) -> String {
    if descriptor.route.agent_id.as_str() == "antigravity" {
        // Antigravity requires a concrete thinking-level model id. Use its
        // default high variant until an explicit reasoning effort is applied
        // by the runtime bridge; already-qualified ids remain unchanged.
        if !["-high", "-medium", "-low"]
            .iter()
            .any(|suffix| model.agent_model_id.ends_with(suffix))
        {
            return format!("{}-high", model.agent_model_id);
        }
    }
    if matches!(
        descriptor.provider_control,
        AgentProviderControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::OpenCodeInlineProvider
        }
    ) || matches!(
        descriptor.model_control,
        AgentModelControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::OpenCodeInlineProvider
        }
    ) {
        let base_provider_id = sanitize_provider_id(
            provider
                .vendor_hint
                .as_deref()
                .unwrap_or_else(|| provider.id.as_str()),
        );
        let (provider_id, _) = opencode_provider_identity(&base_provider_id, model);
        return format!(
            "{provider_id}/{}",
            opencode_model_key(&provider_id, &model.agent_model_id)
        );
    }
    if matches!(
        descriptor.provider_control,
        AgentProviderControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::ZcodeJson
        }
    ) || matches!(
        descriptor.model_control,
        AgentModelControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::ZcodeJson
        }
    ) {
        // zcode-acp-server uses a bare model id for built-in ZCode plans and
        // `providerId\\modelId` for projected third-party providers. Vibex
        // projections are always custom providers, so preserve that ownership
        // in the ACP model option instead of letting the bridge resolve the id
        // against an unrelated built-in plan.
        let provider_id = sanitize_provider_id(
            provider
                .vendor_hint
                .as_deref()
                .unwrap_or_else(|| provider.id.as_str()),
        );
        return format!("{provider_id}\\{}", model.agent_model_id);
    }
    if matches!(
        descriptor.provider_control,
        AgentProviderControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::PiModelsJson
        }
    ) || matches!(
        descriptor.model_control,
        AgentModelControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::PiModelsJson
        }
    ) {
        let provider_id = sanitize_provider_id(
            provider
                .vendor_hint
                .as_deref()
                .unwrap_or_else(|| provider.id.as_str()),
        );
        return format!(
            "{provider_id}/{}",
            model
                .agent_model_id
                .strip_prefix(&format!("{provider_id}/"))
                .unwrap_or(&model.agent_model_id)
        );
    }
    if matches!(
        descriptor.provider_control,
        AgentProviderControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::HermesYaml
        }
    ) || matches!(
        descriptor.model_control,
        AgentModelControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::HermesYaml
        }
    ) {
        // Hermes exposes custom-provider models through ACP as `custom:<model>`
        // regardless of the provider name in config.yaml. Keep this mapping
        // idempotent for callers that already supplied the qualified form.
        let model_id = model
            .agent_model_id
            .strip_prefix("custom:")
            .unwrap_or(&model.agent_model_id);
        return format!("custom:{model_id}");
    }
    model.agent_model_id.clone()
}

fn require_secret_env_key(key: Option<&str>) -> VibexResult<&str> {
    key.map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            VibexError::validation(
                "agent_projection_secret_env_key_missing",
                "typed Agent overlay is missing its code-owned Secret environment key",
            )
        })
}

fn projection_model_id(model: Option<&AgentConfiguredModelBinding>) -> Option<&str> {
    model
        .map(|model| model.agent_model_id.trim())
        .filter(|model| !model.is_empty())
}

fn projection_wire_protocol(model: Option<&AgentConfiguredModelBinding>) -> &str {
    model
        .map(|model| model.wire_protocol_id.as_str())
        .unwrap_or(vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS)
}

fn hermes_api_mode(model: Option<&AgentConfiguredModelBinding>) -> &'static str {
    match projection_wire_protocol(model) {
        vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES => "codex_responses",
        vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => "anthropic_messages",
        vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE => "bedrock_converse",
        _ => "chat_completions",
    }
}

fn mistral_vibe_api_style(model: Option<&AgentConfiguredModelBinding>) -> &'static str {
    match projection_wire_protocol(model) {
        vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES => "openai-responses",
        vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => "anthropic",
        _ => "openai",
    }
}

fn pi_api_kind(model: Option<&AgentConfiguredModelBinding>) -> &'static str {
    match projection_wire_protocol(model) {
        vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES => "openai-responses",
        vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => "anthropic-messages",
        _ => "openai-completions",
    }
}

fn factory_droid_provider_kind(model: Option<&AgentConfiguredModelBinding>) -> &'static str {
    match projection_wire_protocol(model) {
        vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES => "openai",
        vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => "anthropic",
        _ => "generic-chat-completion-api",
    }
}

fn json_string_if_present(
    object: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<&str>,
) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        object.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }
}

fn serialized_json(value: serde_json::Value) -> VibexResult<String> {
    serde_json::to_string(&value).map_err(encode_error)
}

fn serialized_toml(value: serde_json::Value) -> VibexResult<String> {
    toml::to_string(&value).map_err(|error| {
        VibexError::validation(
            "agent_projection_overlay_encode_failed",
            "typed Agent TOML overlay could not be encoded",
        )
        .with_diagnostic("format", "toml")
        .with_diagnostic("error", error.to_string())
    })
}

fn serialized_yaml(value: serde_json::Value) -> VibexResult<String> {
    serde_yaml::to_string(&value).map_err(|error| {
        VibexError::validation(
            "agent_projection_overlay_encode_failed",
            "typed Agent YAML overlay could not be encoded",
        )
        .with_diagnostic("format", "yaml")
        .with_diagnostic("error", error.to_string())
    })
}

fn zcode_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let provider_id = sanitize_provider_id(
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str()),
    );
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let kind = match projection_wire_protocol(model) {
        vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => "anthropic",
        _ => "openai-compatible",
    };
    let mut options = serde_json::Map::new();
    json_string_if_present(
        &mut options,
        "baseURL",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    options.insert(
        "apiKey".to_string(),
        serde_json::Value::String(overlay_secret_placeholder(secret_env_key)),
    );
    options.insert("apiKeyRequired".to_string(), serde_json::Value::Bool(true));

    let capabilities = model.and_then(|binding| {
        provider
            .configured_models
            .iter()
            .find(|configured| configured.id == binding.provider_model_id)
            .map(|configured| &configured.capabilities)
    });
    let mut model_entry = serde_json::Map::new();
    model_entry.insert(
        "name".to_string(),
        serde_json::Value::String(model_id.to_string()),
    );
    if let Some(context) = capabilities.and_then(|value| value.context_tokens) {
        model_entry.insert(
            "limit".to_string(),
            serde_json::json!({ "context": context }),
        );
    }
    if capabilities.and_then(|value| value.reasoning) == Some(true) {
        model_entry.insert(
            "reasoning".to_string(),
            serde_json::json!({ "enabled": true }),
        );
    }

    serialized_json(serde_json::json!({
        "provider": {
            provider_id: {
                "name": provider.display_name,
                "kind": kind,
                "enabled": true,
                "source": "custom",
                "options": options,
                "models": {
                    model_id: model_entry,
                },
            }
        }
    }))
}

fn crow_cli_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let provider_id = sanitize_provider_id(
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str()),
    );
    let mut provider_config = serde_json::Map::new();
    provider_config.insert(
        "base_url".to_string(),
        endpoint.map_or(serde_json::Value::Null, |endpoint| {
            serde_json::Value::String(endpoint.url.clone())
        }),
    );
    provider_config.insert(
        "api_key".to_string(),
        serde_json::Value::String(format!("${{{secret_env_key}}}")),
    );
    let mut providers = serde_json::Map::new();
    providers.insert(
        provider_id.clone(),
        serde_json::Value::Object(provider_config),
    );
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let mut models = serde_json::Map::new();
    models.insert(
        model_id.to_string(),
        serde_json::json!({"provider": provider_id, "model": model_id}),
    );
    serialized_yaml(serde_json::json!({
        "providers": providers,
        "models": models,
    }))
}

fn dirac_overlay(
    _provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
) -> VibexResult<String> {
    let endpoint = endpoint.map(|endpoint| endpoint.url.as_str()).unwrap_or("");
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    serialized_json(serde_json::json!({
        "actModeApiProvider": "openai",
        "planModeApiProvider": "openai",
        "openAiBaseUrl": endpoint,
        "actModeOpenAiModelId": model_id,
        "planModeOpenAiModelId": model_id,
    }))
}

fn factory_droid_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let custom_id = sanitize_provider_id(&format!(
        "vibex-{}-{model_id}",
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str())
    ));
    let mut custom = serde_json::Map::new();
    custom.insert("id".to_string(), serde_json::json!(custom_id));
    custom.insert("model".to_string(), serde_json::json!(model_id));
    json_string_if_present(
        &mut custom,
        "baseUrl",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    custom.insert(
        "provider".to_string(),
        serde_json::json!(factory_droid_provider_kind(model)),
    );
    custom.insert(
        "apiKey".to_string(),
        serde_json::json!(format!("${{{secret_env_key}}}")),
    );
    serialized_json(serde_json::json!({
        "customModels": [custom],
        "model": format!("custom:{custom_id}"),
    }))
}

fn goose_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let provider_id = goose_provider_id(provider);
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let mut root = serde_json::Map::new();
    root.insert("name".to_string(), serde_json::json!(provider_id));
    root.insert("engine".to_string(), serde_json::json!("openai"));
    root.insert(
        "display_name".to_string(),
        serde_json::json!(provider.display_name),
    );
    root.insert("api_key_env".to_string(), serde_json::json!(secret_env_key));
    json_string_if_present(
        &mut root,
        "base_url",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    root.insert(
        "models".to_string(),
        serde_json::json!([{
            "name": model_id,
            "context_limit": 128000,
        }]),
    );
    root.insert("supports_streaming".to_string(), serde_json::json!(true));
    root.insert("requires_auth".to_string(), serde_json::json!(true));
    serialized_json(serde_json::Value::Object(root))
}

fn grok_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let mut config = serde_json::Map::new();
    config.insert("model".to_string(), serde_json::json!(model_id));
    json_string_if_present(
        &mut config,
        "base_url",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    config.insert("name".to_string(), serde_json::json!(provider.display_name));
    config.insert("env_key".to_string(), serde_json::json!(secret_env_key));
    config.insert("api_backend".to_string(), serde_json::json!("responses"));
    serialized_toml(serde_json::json!({
        "model": {model_id: config},
        "models": {"default": model_id},
    }))
}

fn goose_provider_id(provider: &ModelProviderProfile) -> String {
    sanitize_provider_id(
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str()),
    )
}

fn hermes_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let provider_id = sanitize_provider_id(
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str()),
    );
    let mut provider_config = serde_json::Map::new();
    json_string_if_present(
        &mut provider_config,
        "base_url",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    json_string_if_present(&mut provider_config, "name", Some(provider_id.as_str()));
    let secret_placeholder = overlay_secret_placeholder(secret_env_key);
    json_string_if_present(&mut provider_config, "api_key", Some(&secret_placeholder));
    json_string_if_present(
        &mut provider_config,
        "api_mode",
        Some(hermes_api_mode(model)),
    );
    provider_config.insert(
        "models".to_string(),
        serde_json::json!({model_id: {"context_length": 128000}}),
    );
    serialized_yaml(serde_json::json!({
        "custom_providers": [provider_config],
        "model": {"provider": provider_id, "default": model_id},
    }))
}

fn kilo_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let provider_id = sanitize_provider_id(
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str()),
    );
    let mut options = serde_json::Map::new();
    json_string_if_present(
        &mut options,
        "baseURL",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    options.insert(
        "apiKey".to_string(),
        serde_json::json!(format!("{{env:{secret_env_key}}}")),
    );
    let mut model_provider = serde_json::Map::new();
    model_provider.insert(
        "npm".to_string(),
        serde_json::json!("@ai-sdk/openai-compatible"),
    );
    json_string_if_present(
        &mut model_provider,
        "api",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    let mut models = serde_json::Map::new();
    models.insert(
        model_id.to_string(),
        serde_json::json!({
            "name": model_id,
            "provider": model_provider,
        }),
    );
    let mut entry = serde_json::Map::new();
    entry.insert("name".to_string(), serde_json::json!(provider.display_name));
    entry.insert("options".to_string(), serde_json::Value::Object(options));
    entry.insert("models".to_string(), serde_json::Value::Object(models));
    serialized_json(serde_json::json!({
        "provider": {provider_id.clone(): entry},
        "model": format!("{provider_id}/{model_id}"),
    }))
}

fn kimi_auth_compatibility_overlay(secret_env_key: &str) -> ManagedProjectionOverlay {
    ManagedProjectionOverlay {
        relative_path: "credentials/kimi-code.json".to_string(),
        format: "json".to_string(),
        content: serde_json::json!({
            "access_token": overlay_secret_placeholder(secret_env_key),
            "refresh_token": "",
            "expires_at": 0,
            "expires_in": 0,
        })
        .to_string(),
        contains_secret_reference: true,
    }
}

fn kimi_overlay(
    _provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let provider_type = match projection_wire_protocol(model) {
        vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => "anthropic",
        _ => "openai_legacy",
    };
    let endpoint = endpoint
        .map(|endpoint| endpoint.url.as_str())
        .unwrap_or_default();
    let secret_placeholder = overlay_secret_placeholder(secret_env_key);
    let mut root = toml::map::Map::new();
    root.insert(
        "default_model".to_string(),
        toml::Value::String(model_id.to_string()),
    );
    root.insert("default_thinking".to_string(), toml::Value::Boolean(false));
    root.insert(
        "providers".to_string(),
        toml::Value::Table(toml::map::Map::from_iter([(
            "vibex".to_string(),
            toml::Value::Table(toml::map::Map::from_iter([
                (
                    "type".to_string(),
                    toml::Value::String(provider_type.to_string()),
                ),
                (
                    "base_url".to_string(),
                    toml::Value::String(endpoint.to_string()),
                ),
                (
                    "api_key".to_string(),
                    toml::Value::String(secret_placeholder),
                ),
            ])),
        )])),
    );
    root.insert(
        "models".to_string(),
        toml::Value::Table(toml::map::Map::from_iter([(
            model_id.to_string(),
            toml::Value::Table(toml::map::Map::from_iter([
                (
                    "provider".to_string(),
                    toml::Value::String("vibex".to_string()),
                ),
                (
                    "model".to_string(),
                    toml::Value::String(model_id.to_string()),
                ),
                (
                    "max_context_size".to_string(),
                    toml::Value::Integer(262_144),
                ),
                (
                    "capabilities".to_string(),
                    toml::Value::Array(vec![
                        toml::Value::String("thinking".to_string()),
                        toml::Value::String("image_in".to_string()),
                    ]),
                ),
            ])),
        )])),
    );
    toml::to_string(&toml::Value::Table(root)).map_err(|error| {
        VibexError::validation(
            "agent_projection_overlay_encode_failed",
            "managed Kimi TOML overlay could not be encoded",
        )
        .with_diagnostic("format", "toml")
        .with_diagnostic("error", error.to_string())
    })
}

fn mistral_vibe_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let provider_id = sanitize_provider_id(
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str()),
    );
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let mut provider_config = serde_json::Map::new();
    provider_config.insert("name".to_string(), serde_json::json!(provider_id));
    json_string_if_present(
        &mut provider_config,
        "api_base",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    provider_config.insert(
        "api_key_env_var".to_string(),
        serde_json::json!(secret_env_key),
    );
    provider_config.insert(
        "api_style".to_string(),
        serde_json::json!(mistral_vibe_api_style(model)),
    );
    provider_config.insert("backend".to_string(), serde_json::json!("generic"));
    let model_config = serde_json::json!({
        "name": model_id,
        "provider": provider_id,
        "alias": model_id,
    });
    serialized_toml(serde_json::json!({
        "active_model": model_id,
        "providers": [provider_config],
        "models": [model_config],
    }))
}

fn pi_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let provider_id = sanitize_provider_id(
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str()),
    );
    let mut entry = serde_json::Map::new();
    json_string_if_present(
        &mut entry,
        "baseUrl",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    entry.insert(
        "apiKey".to_string(),
        serde_json::json!(format!("${secret_env_key}")),
    );
    entry.insert("api".to_string(), serde_json::json!(pi_api_kind(model)));
    entry.insert(
        "models".to_string(),
        serde_json::json!([{
            "id": model_id,
            "name": model_id,
            "reasoning": pi_model_reasoning(provider, model),
            "input": ["text"],
            "contextWindow": 128000,
            "maxTokens": 16384,
            "cost": {
                "input": 0,
                "output": 0,
                "cacheRead": 0,
                "cacheWrite": 0,
            },
        }]),
    );
    serialized_json(serde_json::json!({
        "providers": {provider_id: entry},
    }))
}

fn pi_model_reasoning(
    provider: &ModelProviderProfile,
    model: Option<&AgentConfiguredModelBinding>,
) -> bool {
    model
        .and_then(|model| {
            provider.configured_models.iter().find(|configured| {
                configured.id == model.provider_model_id || configured.id == model.agent_model_id
            })
        })
        .and_then(|model| model.capabilities.reasoning)
        .unwrap_or(false)
}

fn qwen_code_overlay(
    _provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let mut entry = serde_json::Map::new();
    entry.insert("id".to_string(), serde_json::json!(model_id));
    entry.insert("name".to_string(), serde_json::json!(model_id));
    json_string_if_present(
        &mut entry,
        "baseUrl",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    entry.insert("envKey".to_string(), serde_json::json!(secret_env_key));
    serialized_json(serde_json::json!({
        "modelProviders": {"openai": [entry]},
        "security": {"auth": {"selectedType": "openai"}},
        "model": {"name": model_id},
    }))
}

fn stakpak_overlay(
    _provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let provider_id = "openai";
    let mut provider = serde_json::Map::new();
    provider.insert("type".to_string(), serde_json::json!("openai"));
    json_string_if_present(
        &mut provider,
        "api_endpoint",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    let mut profile = serde_json::Map::new();
    profile.insert("provider".to_string(), serde_json::json!("local"));
    profile.insert(
        "model".to_string(),
        serde_json::json!(format!("{provider_id}/{model_id}")),
    );
    profile.insert(
        "providers".to_string(),
        serde_json::json!({provider_id: provider}),
    );
    let _ = secret_env_key;
    serialized_toml(serde_json::json!({
        "profiles": {"vibex": profile},
    }))
}

fn vtcode_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
    secret_env_key: &str,
) -> VibexResult<String> {
    let model_id = projection_model_id(model).unwrap_or("vibex-model");
    let provider_id = sanitize_provider_id(&format!(
        "vibex-{}",
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str())
    ));
    let mut entry = serde_json::Map::new();
    entry.insert("name".to_string(), serde_json::json!(provider_id));
    entry.insert(
        "display_name".to_string(),
        serde_json::json!(provider.display_name),
    );
    json_string_if_present(
        &mut entry,
        "base_url",
        endpoint.map(|endpoint| endpoint.url.as_str()),
    );
    entry.insert("model".to_string(), serde_json::json!(model_id));
    entry.insert("api_key_env".to_string(), serde_json::json!(secret_env_key));
    serialized_toml(serde_json::json!({
        "agent": {
            "provider": provider_id,
            "default_model": model_id,
        },
        "custom_providers": [entry],
    }))
}

fn structured_map(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
) -> BTreeMap<String, String> {
    let mut values = BTreeMap::from([
        ("provider_id".to_string(), provider.id.as_str().to_string()),
        ("provider_name".to_string(), provider.display_name.clone()),
    ]);
    if let Some(endpoint) = endpoint {
        values.insert("base_url".to_string(), endpoint.url.clone());
    }
    if let Some(model) = model {
        values.insert("model".to_string(), model.agent_model_id.clone());
        values.insert("wire_protocol".to_string(), model.wire_protocol_id.clone());
    }
    values
}

fn structured_json_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
) -> VibexResult<String> {
    serde_json::to_string(&structured_map(provider, endpoint, model)).map_err(encode_error)
}

fn structured_toml_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
) -> VibexResult<String> {
    toml::to_string(&structured_map(provider, endpoint, model)).map_err(|error| {
        VibexError::validation(
            "agent_projection_overlay_encode_failed",
            "managed TOML overlay could not be encoded",
        )
        .with_diagnostic("format", "toml")
        .with_diagnostic("error", error.to_string())
    })
}

fn structured_yaml_overlay(
    provider: &ModelProviderProfile,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
) -> VibexResult<String> {
    serde_yaml::to_string(&structured_map(provider, endpoint, model)).map_err(|error| {
        VibexError::validation(
            "agent_projection_overlay_encode_failed",
            "managed YAML overlay could not be encoded",
        )
        .with_diagnostic("format", "yaml")
        .with_diagnostic("error", error.to_string())
    })
}

#[allow(clippy::too_many_arguments)]
fn projection_fingerprint(
    provider: &ModelProviderProfile,
    runtime: &AgentRuntimeProfile,
    binding: &AgentModelProviderBinding,
    descriptor: &AgentProviderProjectionDescriptor,
    model: Option<&AgentConfiguredModelBinding>,
    env: &BTreeMap<String, String>,
    secrets: &[ProjectionSecretEnvReference],
    overlays: &[ManagedProjectionOverlay],
    runtime_home_env_key: Option<&str>,
    process_args: &[String],
) -> VibexResult<String> {
    let process_model = model.filter(|model| {
        model.process_scoped
            || descriptor.model_interfaces.iter().any(|interface| {
                interface.wire_protocol_id == model.wire_protocol_id
                    && interface.sdk_adapter_id == model.sdk_adapter_id
                    && interface.process_scoped
            })
            || matches!(
                descriptor.model_control,
                AgentModelControl::ProcessEnvironment { .. }
                    | AgentModelControl::ManagedConfigOverlay { .. }
            )
    });
    let secret_revisions = secrets
        .iter()
        .map(|entry| {
            (
                entry.key.clone(),
                format!(
                    "{}:{}:{:?}:{:?}",
                    entry.credential_id,
                    entry.secret_reference.revision,
                    entry.secret_reference.backend,
                    entry.secret_reference.setup_state
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let overlay_fingerprints = overlays
        .iter()
        .map(|overlay| {
            (
                overlay.relative_path.clone(),
                hex_digest(Sha256::digest(overlay.content.as_bytes()).as_slice()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let canonical = serde_json::json!({
        "domain": PROJECTION_FINGERPRINT_DOMAIN,
        "bindingId": binding.id,
        "descriptorId": descriptor.id,
        "descriptorVersion": descriptor.descriptor_version,
        "route": runtime.version_identity.route,
        "adapterVersion": runtime.version_identity.adapter_version,
        "agentVersion": runtime.version_identity.agent_version,
        "runtimeDependencies": runtime.version_identity.runtime_dependencies,
        "command": runtime.command,
        "args": runtime.args,
        "safeEnvReferences": runtime.safe_env_references,
        "runtimeHomeEnvKey": runtime_home_env_key,
        "processArgs": process_args,
        "providerId": provider.id,
        "providerStatus": provider.status,
        "endpoint": selected_endpoint(provider, binding, model)?.map(|value| &value.url),
        "proxyPolicy": provider.proxy_policy,
        "nonSecretEnv": env,
        "secretReferenceRevisions": secret_revisions,
        "overlays": overlay_fingerprints,
        "processModel": process_model,
    });
    let bytes = serde_json::to_vec(&canonical).map_err(encode_error)?;
    Ok(format!(
        "sha256:{}",
        hex_digest(Sha256::digest(bytes).as_slice())
    ))
}

fn load_projection_input(
    conn: &vibex_db::DbConnection,
    binding_id: &AgentModelProviderBindingId,
) -> VibexResult<(
    ModelProviderProfile,
    AgentRuntimeProfile,
    AgentModelProviderBinding,
    AgentProviderProjectionDescriptor,
)> {
    let binding = require_binding(conn, binding_id)?;
    let runtime = require_runtime(conn, &binding.runtime_profile_id)?;
    let provider = require_model_provider(conn, &binding.model_provider_profile_id)?;
    let resolution =
        AgentProviderProjectionRegistry::builtin()?.resolve(&runtime.version_identity)?;
    if binding.projection_descriptor_id != resolution.descriptor.id {
        return Err(VibexError::validation(
            "agent_projection_descriptor_mismatch",
            "persisted binding descriptor does not match the compatible runtime identity",
        ));
    }
    Ok((provider, runtime, binding, resolution.descriptor))
}

fn require_model_provider(
    conn: &vibex_db::DbConnection,
    id: &ModelProviderProfileId,
) -> VibexResult<ModelProviderProfile> {
    ModelProviderProfileRepository::get(conn, id)?.ok_or_else(|| {
        VibexError::validation(
            "model_provider_profile_not_found",
            "model provider profile was not found",
        )
    })
}

fn require_runtime(
    conn: &vibex_db::DbConnection,
    id: &AgentRuntimeProfileId,
) -> VibexResult<AgentRuntimeProfile> {
    AgentRuntimeProfileRepository::get(conn, id)?.ok_or_else(|| {
        VibexError::validation(
            "agent_runtime_profile_not_found",
            "Agent runtime profile was not found",
        )
    })
}

fn require_binding(
    conn: &vibex_db::DbConnection,
    id: &AgentModelProviderBindingId,
) -> VibexResult<AgentModelProviderBinding> {
    AgentModelProviderBindingRepository::get(conn, id)?.ok_or_else(|| {
        VibexError::validation(
            "agent_model_provider_binding_not_found",
            "Agent model provider binding was not found",
        )
    })
}

fn verification_from_descriptor(
    descriptor: &AgentProviderProjectionDescriptor,
) -> ProjectionVerificationState {
    ProjectionVerificationState {
        state: descriptor.evidence.state,
        descriptor_version: descriptor.descriptor_version.clone(),
        source_evidence_reference: descriptor.evidence.source_reference.clone(),
        runtime_evidence_reference: descriptor.evidence.runtime_reference.clone(),
        verified_at_ms: None,
    }
}

fn status_for_resolution(
    match_kind: ProjectionDescriptorMatch,
    descriptor: &AgentProviderProjectionDescriptor,
) -> AgentModelProviderBindingStatus {
    if match_kind == ProjectionDescriptorMatch::Conservative {
        AgentModelProviderBindingStatus::Unverified
    } else {
        match descriptor.evidence.state {
            ProjectionEvidenceState::Verified => AgentModelProviderBindingStatus::Ready,
            ProjectionEvidenceState::Unsupported => AgentModelProviderBindingStatus::Unsupported,
            ProjectionEvidenceState::Documented
            | ProjectionEvidenceState::AgentManaged
            | ProjectionEvidenceState::Local
            | ProjectionEvidenceState::ServiceMarketplace
            | ProjectionEvidenceState::Unverified
            | ProjectionEvidenceState::Stale => AgentModelProviderBindingStatus::Unverified,
        }
    }
}

fn apply_stale_state(
    binding: &mut AgentModelProviderBinding,
    next_fingerprint: &str,
    match_kind: ProjectionDescriptorMatch,
) {
    if binding.verification.state == ProjectionEvidenceState::Unsupported {
        binding.status = AgentModelProviderBindingStatus::Unsupported;
        binding.projection_fingerprint = None;
        return;
    }
    if match_kind == ProjectionDescriptorMatch::Conservative
        || binding.verification.state != ProjectionEvidenceState::Verified
    {
        binding.status = AgentModelProviderBindingStatus::Unverified;
        binding.projection_fingerprint = None;
        if match_kind == ProjectionDescriptorMatch::Conservative {
            binding.verification.state = ProjectionEvidenceState::Unverified;
        }
        return;
    }
    match binding.projection_fingerprint.as_deref() {
        None => {
            binding.projection_fingerprint = Some(next_fingerprint.to_string());
            binding.status = AgentModelProviderBindingStatus::Ready;
        }
        Some(active) if active == next_fingerprint => {
            binding.status = AgentModelProviderBindingStatus::Ready;
        }
        Some(_) => {
            binding.status = AgentModelProviderBindingStatus::StaleRestartRequired;
            binding.verification.state = ProjectionEvidenceState::Stale;
        }
    }
}

fn persist_projection_state_if_changed(
    conn: &vibex_db::DbConnection,
    binding: &AgentModelProviderBinding,
    fingerprint: Option<&str>,
) -> VibexResult<bool> {
    let existing = require_binding(conn, &binding.id)?;
    if existing.projection_fingerprint.as_deref() == fingerprint
        && existing.status == binding.status
        && existing.verification == binding.verification
    {
        return Ok(false);
    }
    AgentModelProviderBindingRepository::set_projection_state(
        conn,
        &binding.id,
        existing.revision,
        fingerprint,
        binding.status,
        &binding.verification,
        unix_timestamp_ms().max(existing.updated_at_ms.saturating_add(1)),
    )?;
    Ok(true)
}

fn projection_auth_state(provider: &ModelProviderProfile) -> ProjectionAuthState {
    if provider.credentials.is_empty() {
        return ProjectionAuthState::Missing;
    }
    if provider.credentials.iter().any(|credential| {
        matches!(
            credential.credential.kind(),
            AgentCredentialKind::OAuth | AgentCredentialKind::ManagedSubscription
        )
    }) {
        return ProjectionAuthState::AgentManaged;
    }
    if provider.credentials.iter().any(|credential| {
        matches!(
            credential.status,
            AgentCredentialStatus::Ready | AgentCredentialStatus::Referenced
        )
    }) {
        ProjectionAuthState::Ready
    } else {
        ProjectionAuthState::Missing
    }
}

fn replace_credential_secret(
    credential: &mut AgentCredential,
    secret: vibex_core::ProjectionSecretReference,
) -> VibexResult<()> {
    match credential {
        AgentCredential::ApiKey { secret: value, .. } => *value = secret,
        AgentCredential::Aws { secret: value, .. } => *value = Some(secret),
        AgentCredential::Gcp {
            credential: value, ..
        }
        | AgentCredential::Azure {
            credential: value, ..
        }
        | AgentCredential::Snowflake {
            credential: value, ..
        } => *value = Some(secret),
        AgentCredential::OAuth { .. }
        | AgentCredential::Local { .. }
        | AgentCredential::ManagedSubscription { .. } => {
            return Err(VibexError::validation(
                "model_provider_credential_secret_unsupported",
                "selected credential does not use a host-managed Secret value",
            ));
        }
    }
    Ok(())
}

fn validate_workspace_key(value: &str) -> VibexResult<()> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_WORKSPACE_KEY_LEN
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err(VibexError::validation(
            "agent_projection_workspace_key_invalid",
            "projection workspace key must be a bounded, non-path stable identity",
        ));
    }
    Ok(())
}

fn projection_overlay_root(
    runtime_root: &Path,
    binding_id: &AgentModelProviderBindingId,
    workspace_key: &str,
) -> PathBuf {
    let workspace_digest = hex_digest(Sha256::digest(workspace_key.as_bytes()).as_slice());
    runtime_root
        .join(binding_id.as_str())
        .join(format!("workspace_{}", &workspace_digest[..32]))
}

fn ensure_private_directory(path: &Path) -> VibexResult<()> {
    if reject_symlink_if_present(path)? {
        if !path.is_dir() {
            return Err(VibexError::validation(
                "agent_projection_runtime_directory_invalid",
                "managed projection runtime path is not a directory",
            ));
        }
    } else {
        fs::create_dir_all(path).map_err(|error| {
            VibexError::storage(
                "agent_projection_runtime_directory_create_failed",
                "failed to create private projection runtime directory",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            VibexError::storage(
                "agent_projection_runtime_directory_permissions_failed",
                "failed to apply private projection runtime directory permissions",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    }
    Ok(())
}

fn safe_overlay_path(root: &Path, relative_path: &str) -> VibexResult<PathBuf> {
    let relative = Path::new(relative_path);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(VibexError::validation(
            "agent_projection_overlay_path_unsafe",
            "managed projection overlay path must remain inside the private runtime root",
        ));
    }
    let mut path = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            unreachable!("components were validated above");
        };
        path.push(component);
        if index + 1 < components.len() {
            ensure_private_directory(&path)?;
        }
    }
    reject_symlink_if_present(&path)?;
    Ok(path)
}

fn reject_symlink_if_present(path: &Path) -> VibexResult<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(VibexError::validation(
            "agent_projection_overlay_symlink_rejected",
            "managed projection paths must not traverse symlinks",
        )),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(VibexError::storage(
            "agent_projection_overlay_metadata_failed",
            "failed to inspect managed projection path",
        )
        .with_diagnostic("error", error.to_string())),
    }
}

fn write_private_file_atomic(path: &Path, content: &[u8]) -> VibexResult<()> {
    let parent = path.parent().ok_or_else(|| {
        VibexError::validation(
            "agent_projection_overlay_parent_missing",
            "managed projection overlay has no parent directory",
        )
    })?;
    let temp = parent.join(format!(".projection-{}.tmp", RequestId::new().as_str()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|error| {
        VibexError::storage(
            "agent_projection_overlay_temp_create_failed",
            "failed to create private projection overlay temporary file",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let write_result = file
        .write_all(content)
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&temp, path));
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(VibexError::storage(
            "agent_projection_overlay_write_failed",
            "failed to atomically write private projection overlay",
        )
        .with_diagnostic("error", error.to_string()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            VibexError::storage(
                "agent_projection_overlay_permissions_failed",
                "failed to apply private projection overlay permissions",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    }
    Ok(())
}

fn endpoint_target(key: &str, value: &str) -> ProjectionTargetPreview {
    ProjectionTargetPreview {
        field: "endpoint".to_string(),
        target_kind: ProjectionTargetKind::Environment,
        target: key.to_string(),
        value_preview: redact_endpoint(value),
        secret: false,
    }
}

fn state_target(kind: ProjectionTargetKind, target: &str) -> ProjectionTargetPreview {
    ProjectionTargetPreview {
        field: "state".to_string(),
        target_kind: kind,
        target: target.to_string(),
        value_preview: "managed outside automatic projection".to_string(),
        secret: false,
    }
}

fn model_target(
    kind: ProjectionTargetKind,
    target: &str,
    model: &AgentConfiguredModelBinding,
) -> ProjectionTargetPreview {
    ProjectionTargetPreview {
        field: "model".to_string(),
        target_kind: kind,
        target: target.to_string(),
        value_preview: model.agent_model_id.clone(),
        secret: false,
    }
}

fn metadata(key: &str, value: &str) -> ProviderBindingMetadata {
    ProviderBindingMetadata {
        key: key.to_string(),
        value: value.to_string(),
    }
}

fn command_summary(runtime: &AgentRuntimeProfile) -> String {
    let command = Path::new(&runtime.command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("managed-agent");
    format!("{command} ({} argument(s))", runtime.args.len())
}

fn redact_endpoint(value: &str) -> String {
    reqwest::Url::parse(value).map_or_else(
        |_| "configured endpoint".to_string(),
        |url| {
            let host = url.host_str().unwrap_or("configured-host");
            let port = url
                .port()
                .map(|port| format!(":{port}"))
                .unwrap_or_default();
            format!("{}://{host}{port}", url.scheme())
        },
    )
}

fn sanitize_provider_id(value: &str) -> String {
    let mut output = String::new();
    for character in value.trim().to_ascii_lowercase().chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            output.push(character);
        } else if !output.ends_with('-') && !output.is_empty() {
            output.push('-');
        }
        if output.len() >= 80 {
            break;
        }
    }
    let output = output.trim_matches(['-', '_']);
    if output.is_empty() {
        "vibex".to_string()
    } else {
        output.to_string()
    }
}

fn toml_quote(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

fn encode_error(error: serde_json::Error) -> VibexError {
    VibexError::validation(
        "agent_projection_overlay_encode_failed",
        "managed projection overlay could not be encoded",
    )
    .with_diagnostic("format", "json")
    .with_diagnostic("error", error.to_string())
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;
    use vibex_core::{
        AcpAdapterId, AcpProcessStrategy, AgentConfiguredModelBindingId, AgentHostCapabilities,
        AgentId, AgentModelInterfaceDescriptor, AgentProviderProjectionDescriptorId,
        AgentProviderProjectionOverrides, AgentRuntimeHomeStrategy, AgentRuntimeResourcePolicy,
        AgentRuntimeRouteKey, AgentRuntimeVersionIdentity, AgentVersionCompatibility,
        AgentVersionSource, ModelProviderCatalogEntry, ModelProviderEndpointKind,
        ModelProviderProfileStatus, ProjectionEvidenceReference, ProjectionSecretReference,
        ProviderNetworkDefaults, ProviderPermissionDefaults, ProviderSandboxDefaults,
        ProviderSecretKind, ProviderSecretSetupState, TransportKind,
        WIRE_PROTOCOL_OPENAI_RESPONSES,
    };

    use super::*;

    #[test]
    fn zcode_overlay_projects_private_provider_registry_config() {
        let (mut provider, _, binding, _) = fixture(ConfigOverlayStrategy::ZcodeJson);
        provider.endpoints[0].wire_protocol_id =
            Some(vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string());
        let mut model = binding.configured_models[0].clone();
        model.wire_protocol_id = vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string();

        let overlay = zcode_overlay(
            &provider,
            provider.endpoints.first(),
            Some(&model),
            "ANTHROPIC_API_KEY",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&overlay).unwrap();
        let projected = &value["provider"]["fake"];

        assert_eq!(projected["kind"], "anthropic");
        assert_eq!(projected["enabled"], true);
        assert_eq!(
            projected["options"]["baseURL"],
            "https://user:pass@example.invalid/v1?token=never-preview"
        );
        assert_eq!(
            projected["options"]["apiKey"],
            overlay_secret_placeholder("ANTHROPIC_API_KEY")
        );
        assert!(projected["models"]["model-a"].is_object());

        let mut descriptor = fixture(ConfigOverlayStrategy::ZcodeJson).3;
        descriptor.route.agent_id = AgentId::parse("zcode").unwrap();
        descriptor.provider_control = AgentProviderControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::ZcodeJson,
        };
        descriptor.model_control = AgentModelControl::ManagedConfigOverlay {
            strategy: ConfigOverlayStrategy::ZcodeJson,
        };
        assert_eq!(
            projected_runtime_model_id(&provider, &descriptor, &model),
            "fake\\model-a"
        );
    }

    fn fixture(
        strategy: ConfigOverlayStrategy,
    ) -> (
        ModelProviderProfile,
        AgentRuntimeProfile,
        AgentModelProviderBinding,
        AgentProviderProjectionDescriptor,
    ) {
        let now = 1;
        let route = AgentRuntimeRouteKey {
            agent_id: AgentId::parse("fake-agent").unwrap(),
            transport_kind: TransportKind::Acp,
            adapter_id: AcpAdapterId::parse("fake-adapter").unwrap(),
        };
        let provider = ModelProviderProfile {
            id: ModelProviderProfileId::new(),
            legacy_provider_profile_id: None,
            display_name: "Fake Provider".to_string(),
            vendor_hint: Some("fake".to_string()),
            endpoints: vec![ModelProviderEndpoint {
                id: "api".to_string(),
                kind: ModelProviderEndpointKind::Api,
                url: "https://user:pass@example.invalid/v1?token=never-preview".to_string(),
                wire_protocol_id: None,
            }],
            proxy_policy: ModelProviderProxyPolicy::InheritSystem,
            credentials: Vec::new(),
            configured_models: vec![ModelProviderCatalogEntry {
                id: "model-a".to_string(),
                display_name: None,
                enabled: true,
                metadata: Vec::new(),
                capabilities: Default::default(),
            }],
            default_model_id: Some("model-a".to_string()),
            headers: Vec::new(),
            status: ModelProviderProfileStatus::Enabled,
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };
        let runtime = AgentRuntimeProfile {
            id: AgentRuntimeProfileId::new(),
            legacy_provider_profile_id: None,
            version_identity: AgentRuntimeVersionIdentity {
                route: route.clone(),
                adapter_version: Some("1.0.0".to_string()),
                agent_version: None,
                runtime_dependencies: BTreeMap::new(),
                source: AgentVersionSource::Managed,
            },
            command: "/private/user/bin/fake-agent".to_string(),
            args: vec!["acp".to_string()],
            safe_env_references: Vec::new(),
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: AcpProcessStrategy::PerSession,
            runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
            host_capabilities: AgentHostCapabilities::default(),
            resource_policy: AgentRuntimeResourcePolicy {
                sandbox: ProviderSandboxDefaults::workspace_write_ask_on_risk(),
                network: ProviderNetworkDefaults::local_default(),
                permissions: ProviderPermissionDefaults::ask_on_risk(),
            },
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };
        let descriptor = AgentProviderProjectionDescriptor {
            id: AgentProviderProjectionDescriptorId::parse("projection_fake_config_v1").unwrap(),
            descriptor_version: "1".to_string(),
            route,
            compatibility: AgentVersionCompatibility::Exact {
                adapter_version: Some("1.0.0".to_string()),
                agent_version: None,
                runtime_dependencies: BTreeMap::new(),
            },
            provider_control: AgentProviderControl::ManagedConfigOverlay { strategy },
            credential_control: AgentCredentialControl::Unsupported,
            model_control: AgentModelControl::ManagedConfigOverlay {
                strategy: ConfigOverlayStrategy::StructuredJsonOverlay,
            },
            credential_kinds: Vec::new(),
            model_interfaces: vec![AgentModelInterfaceDescriptor {
                wire_protocol_id: WIRE_PROTOCOL_OPENAI_RESPONSES.to_string(),
                sdk_adapter_id: None,
                transport: "https".to_string(),
                integration_kind: vibex_core::AgentModelInterfaceIntegrationKind::Direct,
                user_selectable: false,
                process_scoped: true,
            }],
            runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
            switch_behavior: ProviderSwitchBehavior::RestartAndResume,
            evidence: ProjectionEvidenceReference {
                state: ProjectionEvidenceState::Documented,
                source_reference: Some("test/source".to_string()),
                runtime_reference: None,
                diagnostic_code: None,
            },
        };
        let binding = AgentModelProviderBinding {
            id: AgentModelProviderBindingId::new(),
            legacy_provider_profile_id: None,
            agent_id: descriptor.route.agent_id.clone(),
            runtime_profile_id: runtime.id.clone(),
            model_provider_profile_id: provider.id.clone(),
            projection_descriptor_id: descriptor.id.clone(),
            projection_overrides: AgentProviderProjectionOverrides::default(),
            configured_models: vec![AgentConfiguredModelBinding {
                id: AgentConfiguredModelBindingId::new(),
                provider_model_id: "model-a".to_string(),
                agent_model_id: "model-a".to_string(),
                wire_protocol_id: WIRE_PROTOCOL_OPENAI_RESPONSES.to_string(),
                sdk_adapter_id: None,
                deployment: None,
                enabled: true,
                process_scoped: true,
            }],
            projection_fingerprint: None,
            status: AgentModelProviderBindingStatus::Ready,
            verification: verification_from_descriptor(&descriptor),
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };
        (provider, runtime, binding, descriptor)
    }

    #[test]
    fn codex_overlay_identifies_the_codex_http_engine_to_compatible_gateways() {
        let (provider, _, _, _) = fixture(ConfigOverlayStrategy::CodexStableHome);
        let endpoint = provider.endpoints.first().unwrap();
        let model = AgentConfiguredModelBinding {
            id: AgentConfiguredModelBindingId::new(),
            provider_model_id: "model-a".to_string(),
            agent_model_id: "custom-model-a".to_string(),
            wire_protocol_id: WIRE_PROTOCOL_OPENAI_RESPONSES.to_string(),
            sdk_adapter_id: None,
            deployment: None,
            enabled: true,
            process_scoped: true,
        };
        let content = codex_overlay(&provider, Some(endpoint), Some(&model));
        let config: toml::Value = toml::from_str(&content).unwrap();
        let projected = &config["model_providers"]["fake"];

        assert_eq!(config["model"].as_str(), Some("custom-model-a"));
        assert_eq!(
            projected["http_headers"]["originator"].as_str(),
            Some(CODEX_PROVIDER_ORIGINATOR)
        );
        assert_eq!(projected["requires_openai_auth"].as_bool(), Some(true));
        assert_eq!(projected["env_key"].as_str(), Some("CODEX_API_KEY"));
    }

    const TYPED_SECRET_LOOKUP: &str = "opaque-typed-projection-secret";
    const TYPED_SECRET_SENTINEL: &str = "typed-projection-secret-must-not-leak";

    #[derive(Debug, Clone, Copy)]
    struct TypedProjectionExpectation {
        agent_id: &'static str,
        base_url_key: Option<&'static str>,
        secret_env_key: &'static str,
        model_env_key: Option<&'static str>,
        overlay_path: Option<&'static str>,
        overlay_format: Option<&'static str>,
        runtime_home_env_key: Option<&'static str>,
    }

    fn typed_projection_expectations() -> [TypedProjectionExpectation; 18] {
        [
            TypedProjectionExpectation {
                agent_id: "antigravity",
                base_url_key: Some("GOOGLE_GEMINI_BASE_URL"),
                secret_env_key: "GEMINI_API_KEY",
                model_env_key: None,
                overlay_path: None,
                overlay_format: None,
                runtime_home_env_key: Some("GEMINI_HOME"),
            },
            TypedProjectionExpectation {
                agent_id: "copilot",
                base_url_key: Some("COPILOT_PROVIDER_BASE_URL"),
                secret_env_key: "COPILOT_PROVIDER_API_KEY",
                model_env_key: Some("COPILOT_MODEL"),
                overlay_path: None,
                overlay_format: None,
                runtime_home_env_key: Some("COPILOT_HOME"),
            },
            TypedProjectionExpectation {
                agent_id: "codewhale",
                base_url_key: Some("CODEWHALE_BASE_URL"),
                secret_env_key: "OPENAI_API_KEY",
                model_env_key: Some("CODEWHALE_MODEL"),
                overlay_path: None,
                overlay_format: None,
                runtime_home_env_key: Some("CODEWHALE_HOME"),
            },
            TypedProjectionExpectation {
                agent_id: "crow-cli",
                base_url_key: None,
                secret_env_key: "VIBEX_CROW_API_KEY",
                model_env_key: None,
                overlay_path: Some("config.yaml"),
                overlay_format: Some("yaml"),
                runtime_home_env_key: None,
            },
            TypedProjectionExpectation {
                agent_id: "dirac",
                base_url_key: None,
                secret_env_key: "OPENAI_API_KEY",
                model_env_key: None,
                overlay_path: Some("data/globalState.json"),
                overlay_format: Some("json"),
                runtime_home_env_key: Some("DIRAC_DIR"),
            },
            TypedProjectionExpectation {
                agent_id: "factory-droid",
                base_url_key: None,
                secret_env_key: "VIBEX_FACTORY_DROID_API_KEY",
                model_env_key: None,
                overlay_path: Some("settings.json"),
                overlay_format: Some("json"),
                runtime_home_env_key: Some("FACTORY_HOME_OVERRIDE"),
            },
            TypedProjectionExpectation {
                agent_id: "gemini",
                base_url_key: Some("GOOGLE_GEMINI_BASE_URL"),
                secret_env_key: "GEMINI_API_KEY",
                model_env_key: Some("GEMINI_MODEL"),
                overlay_path: None,
                overlay_format: None,
                runtime_home_env_key: Some("GEMINI_HOME"),
            },
            TypedProjectionExpectation {
                agent_id: "goose",
                base_url_key: None,
                secret_env_key: "VIBEX_GOOSE_API_KEY",
                model_env_key: None,
                overlay_path: Some("config/custom_providers/vibex.json"),
                overlay_format: Some("json"),
                runtime_home_env_key: Some("GOOSE_PATH_ROOT"),
            },
            TypedProjectionExpectation {
                agent_id: "grok",
                base_url_key: None,
                secret_env_key: "VIBEX_GROK_API_KEY",
                model_env_key: None,
                overlay_path: Some("config.toml"),
                overlay_format: Some("toml"),
                runtime_home_env_key: Some("GROK_HOME"),
            },
            TypedProjectionExpectation {
                agent_id: "hermes",
                base_url_key: None,
                secret_env_key: "VIBEX_HERMES_API_KEY",
                model_env_key: None,
                overlay_path: Some("config.yaml"),
                overlay_format: Some("yaml"),
                runtime_home_env_key: Some("HERMES_HOME"),
            },
            TypedProjectionExpectation {
                agent_id: "kilo",
                base_url_key: None,
                secret_env_key: "VIBEX_KILO_API_KEY",
                model_env_key: None,
                overlay_path: Some("kilo.json"),
                overlay_format: Some("json"),
                runtime_home_env_key: None,
            },
            TypedProjectionExpectation {
                agent_id: "kimi",
                base_url_key: None,
                secret_env_key: "VIBEX_KIMI_API_KEY",
                model_env_key: None,
                overlay_path: Some("config.toml"),
                overlay_format: Some("toml"),
                runtime_home_env_key: Some("KIMI_SHARE_DIR"),
            },
            TypedProjectionExpectation {
                agent_id: "mistral-vibe",
                base_url_key: None,
                secret_env_key: "VIBEX_MISTRAL_VIBE_API_KEY",
                model_env_key: None,
                overlay_path: Some("config.toml"),
                overlay_format: Some("toml"),
                runtime_home_env_key: Some("VIBE_HOME"),
            },
            TypedProjectionExpectation {
                agent_id: "poolside",
                base_url_key: Some("POOLSIDE_STANDALONE_BASE_URL"),
                secret_env_key: "POOLSIDE_API_KEY",
                model_env_key: Some("POOLSIDE_STANDALONE_MODEL"),
                overlay_path: None,
                overlay_format: None,
                runtime_home_env_key: None,
            },
            TypedProjectionExpectation {
                agent_id: "pi",
                base_url_key: None,
                secret_env_key: "VIBEX_PI_API_KEY",
                model_env_key: None,
                overlay_path: Some("models.json"),
                overlay_format: Some("json"),
                runtime_home_env_key: Some("PI_CODING_AGENT_DIR"),
            },
            TypedProjectionExpectation {
                agent_id: "qwen-code",
                base_url_key: None,
                secret_env_key: "VIBEX_QWEN_CODE_API_KEY",
                model_env_key: None,
                overlay_path: Some("settings.json"),
                overlay_format: Some("json"),
                runtime_home_env_key: Some("QWEN_HOME"),
            },
            TypedProjectionExpectation {
                agent_id: "stakpak",
                base_url_key: None,
                secret_env_key: "OPENAI_API_KEY",
                model_env_key: None,
                overlay_path: Some("stakpak.toml"),
                overlay_format: Some("toml"),
                runtime_home_env_key: None,
            },
            TypedProjectionExpectation {
                agent_id: "vtcode",
                base_url_key: None,
                secret_env_key: "VIBEX_VTCODE_API_KEY",
                model_env_key: None,
                overlay_path: Some("vtcode.toml"),
                overlay_format: Some("toml"),
                runtime_home_env_key: Some("VTCODE_CONFIG_PATH"),
            },
        ]
    }

    fn typed_projection_fixture(
        descriptor: &AgentProviderProjectionDescriptor,
    ) -> (
        ModelProviderProfile,
        AgentRuntimeProfile,
        AgentModelProviderBinding,
    ) {
        let now = 1;
        let provider_id = ModelProviderProfileId::new();
        let runtime_id = AgentRuntimeProfileId::new();
        let credential_id = RequestId::new();
        let interface = descriptor
            .model_interfaces
            .first()
            .expect("typed projector has a model interface");
        let provider = ModelProviderProfile {
            id: provider_id.clone(),
            legacy_provider_profile_id: None,
            display_name: "Matrix Provider".to_string(),
            vendor_hint: Some("matrix-provider".to_string()),
            endpoints: vec![ModelProviderEndpoint {
                id: "api".to_string(),
                kind: ModelProviderEndpointKind::Api,
                url: "https://provider.example.invalid/v1/".to_string(),
                wire_protocol_id: None,
            }],
            proxy_policy: ModelProviderProxyPolicy::InheritSystem,
            credentials: vec![ModelProviderCredentialReference {
                id: credential_id.clone(),
                display_name: "Matrix API key".to_string(),
                status: AgentCredentialStatus::Referenced,
                credential: AgentCredential::ApiKey {
                    secret: ProjectionSecretReference {
                        id: credential_id,
                        backend: ProviderSecretBackend::Placeholder,
                        setup_state: ProviderSecretSetupState::Missing,
                        lookup_key: TYPED_SECRET_LOOKUP.to_string(),
                        redacted_hint: "configured".to_string(),
                        revision: 7,
                        legacy_secret_reference_id: None,
                    },
                    target_hint: Some(interface.wire_protocol_id.clone()),
                },
                revision: 1,
            }],
            configured_models: vec![ModelProviderCatalogEntry {
                id: "provider-model".to_string(),
                display_name: Some("Matrix Model".to_string()),
                enabled: true,
                metadata: Vec::new(),
                capabilities: Default::default(),
            }],
            default_model_id: Some("provider-model".to_string()),
            headers: Vec::new(),
            status: ModelProviderProfileStatus::Enabled,
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };
        let runtime = AgentRuntimeProfile {
            id: runtime_id.clone(),
            legacy_provider_profile_id: None,
            version_identity: AgentRuntimeVersionIdentity {
                route: descriptor.route.clone(),
                adapter_version: Some("1.0.0".to_string()),
                agent_version: Some("1.0.0".to_string()),
                runtime_dependencies: BTreeMap::new(),
                source: AgentVersionSource::Detected,
            },
            command: format!("/managed/{}", descriptor.route.agent_id),
            args: vec!["acp".to_string()],
            safe_env_references: Vec::new(),
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: AcpProcessStrategy::PerSession,
            runtime_home_strategy: descriptor.runtime_home_strategy.clone(),
            host_capabilities: AgentHostCapabilities::default(),
            resource_policy: AgentRuntimeResourcePolicy {
                sandbox: ProviderSandboxDefaults::workspace_write_ask_on_risk(),
                network: ProviderNetworkDefaults::local_default(),
                permissions: ProviderPermissionDefaults::ask_on_risk(),
            },
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };
        let binding = AgentModelProviderBinding {
            id: vibex_core::AgentModelProviderBindingId::new(),
            legacy_provider_profile_id: None,
            agent_id: descriptor.route.agent_id.clone(),
            runtime_profile_id: runtime_id,
            model_provider_profile_id: provider_id,
            projection_descriptor_id: descriptor.id.clone(),
            projection_overrides: AgentProviderProjectionOverrides::default(),
            configured_models: vec![AgentConfiguredModelBinding {
                id: AgentConfiguredModelBindingId::new(),
                provider_model_id: "provider-model".to_string(),
                agent_model_id: "agent-model".to_string(),
                wire_protocol_id: interface.wire_protocol_id.clone(),
                sdk_adapter_id: interface.sdk_adapter_id.clone(),
                deployment: None,
                enabled: true,
                process_scoped: interface.process_scoped,
            }],
            projection_fingerprint: None,
            status: AgentModelProviderBindingStatus::Ready,
            verification: ProjectionVerificationState {
                state: descriptor.evidence.state,
                descriptor_version: descriptor.descriptor_version.clone(),
                source_evidence_reference: descriptor.evidence.source_reference.clone(),
                runtime_evidence_reference: descriptor.evidence.runtime_reference.clone(),
                verified_at_ms: None,
            },
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };
        (provider, runtime, binding)
    }

    fn assert_overlay_schema(agent_id: &str, overlay: &ManagedProjectionOverlay) {
        match overlay.format.as_str() {
            "json" => {
                let value: serde_json::Value = serde_json::from_str(&overlay.content).unwrap();
                assert!(
                    value.is_object(),
                    "{agent_id} JSON overlay must be an object"
                );
            }
            "toml" => {
                let value: toml::Value = toml::from_str(&overlay.content).unwrap();
                assert!(value.is_table(), "{agent_id} TOML overlay must be a table");
            }
            "yaml" => {
                let value: serde_yaml::Value = serde_yaml::from_str(&overlay.content).unwrap();
                assert!(
                    value.is_mapping(),
                    "{agent_id} YAML overlay must be a mapping"
                );
            }
            format => panic!("unexpected typed overlay format {format}"),
        }
        assert!(!overlay.content.contains(TYPED_SECRET_LOOKUP));
        assert!(!overlay.content.contains(TYPED_SECRET_SENTINEL));
    }

    fn assert_typed_overlay_contract(agent_id: &str, overlay: &ManagedProjectionOverlay) {
        const ENDPOINT: &str = "https://provider.example.invalid/v1/";
        match agent_id {
            "crow-cli" => {
                let value: serde_yaml::Value = serde_yaml::from_str(&overlay.content).unwrap();
                assert_eq!(value["providers"]["matrix-provider"]["base_url"], ENDPOINT);
                assert_eq!(
                    value["providers"]["matrix-provider"]["api_key"],
                    "${VIBEX_CROW_API_KEY}"
                );
                assert_eq!(
                    value["models"]["agent-model"]["provider"],
                    "matrix-provider"
                );
                assert_eq!(value["models"]["agent-model"]["model"], "agent-model");
            }
            "dirac" => {
                let value: serde_json::Value = serde_json::from_str(&overlay.content).unwrap();
                assert_eq!(value["actModeApiProvider"], "openai");
                assert_eq!(value["planModeApiProvider"], "openai");
                assert_eq!(value["openAiBaseUrl"], ENDPOINT);
                assert_eq!(value["actModeOpenAiModelId"], "agent-model");
                assert_eq!(value["planModeOpenAiModelId"], "agent-model");
            }
            "factory-droid" => {
                let value: serde_json::Value = serde_json::from_str(&overlay.content).unwrap();
                let custom = &value["customModels"][0];
                assert_eq!(custom["id"], "vibex-matrix-provider-agent-model");
                assert_eq!(custom["model"], "agent-model");
                assert_eq!(custom["baseUrl"], ENDPOINT);
                assert_eq!(custom["provider"], "openai");
                assert_eq!(custom["apiKey"], "${VIBEX_FACTORY_DROID_API_KEY}");
                assert_eq!(value["model"], "custom:vibex-matrix-provider-agent-model");
                assert!(custom.get("name").is_none());
                assert!(custom.get("protocol").is_none());
                assert!(value.get("defaultModel").is_none());
            }
            "goose" => {
                let value: serde_json::Value = serde_json::from_str(&overlay.content).unwrap();
                assert_eq!(value["name"], "matrix-provider");
                assert_eq!(value["engine"], "openai");
                assert_eq!(value["api_key_env"], "VIBEX_GOOSE_API_KEY");
                assert_eq!(value["base_url"], ENDPOINT);
                assert_eq!(value["models"][0]["name"], "agent-model");
            }
            "grok" => {
                let value: toml::Value = toml::from_str(&overlay.content).unwrap();
                let model = &value["model"]["agent-model"];
                assert_eq!(value["models"]["default"].as_str(), Some("agent-model"));
                assert_eq!(model["model"].as_str(), Some("agent-model"));
                assert_eq!(model["base_url"].as_str(), Some(ENDPOINT));
                assert_eq!(model["env_key"].as_str(), Some("VIBEX_GROK_API_KEY"));
                assert_eq!(model["api_backend"].as_str(), Some("responses"));
                assert!(model.get("auth_scheme").is_none());
            }
            "hermes" => {
                let value: serde_yaml::Value = serde_yaml::from_str(&overlay.content).unwrap();
                let provider = &value["custom_providers"][0];
                assert_eq!(provider["base_url"], ENDPOINT);
                assert_eq!(provider["name"], "matrix-provider");
                assert_eq!(
                    provider["api_key"],
                    overlay_secret_placeholder("VIBEX_HERMES_API_KEY")
                );
                assert_eq!(provider["api_mode"], "chat_completions");
                assert_eq!(provider["models"]["agent-model"]["context_length"], 128000);
                assert_eq!(value["model"]["provider"], "matrix-provider");
                assert_eq!(value["model"]["default"], "agent-model");
                assert!(value.get("providers").is_none());
            }
            "kilo" => {
                let value: serde_json::Value = serde_json::from_str(&overlay.content).unwrap();
                let provider = &value["provider"]["matrix-provider"];
                assert_eq!(provider["options"]["baseURL"], ENDPOINT);
                assert_eq!(provider["options"]["apiKey"], "{env:VIBEX_KILO_API_KEY}");
                assert_eq!(provider["models"]["agent-model"]["name"], "agent-model");
                assert_eq!(
                    provider["models"]["agent-model"]["provider"]["npm"],
                    "@ai-sdk/openai-compatible"
                );
                assert_eq!(
                    provider["models"]["agent-model"]["provider"]["api"],
                    ENDPOINT
                );
                assert_eq!(value["model"], "matrix-provider/agent-model");
                assert!(value.get("providers").is_none());
            }
            "mistral-vibe" => {
                let value: toml::Value = toml::from_str(&overlay.content).unwrap();
                assert_eq!(value["active_model"].as_str(), Some("agent-model"));
                assert_eq!(
                    value["providers"][0]["name"].as_str(),
                    Some("matrix-provider")
                );
                assert_eq!(value["providers"][0]["api_base"].as_str(), Some(ENDPOINT));
                assert_eq!(
                    value["providers"][0]["api_key_env_var"].as_str(),
                    Some("VIBEX_MISTRAL_VIBE_API_KEY")
                );
                assert_eq!(value["providers"][0]["api_style"].as_str(), Some("openai"));
                assert_eq!(
                    value["models"][0]["provider"].as_str(),
                    Some("matrix-provider")
                );
            }
            "pi" => {
                let value: serde_json::Value = serde_json::from_str(&overlay.content).unwrap();
                let provider = &value["providers"]["matrix-provider"];
                assert_eq!(provider["baseUrl"], ENDPOINT);
                assert_eq!(provider["apiKey"], "$VIBEX_PI_API_KEY");
                assert_eq!(provider["api"], "openai-completions");
                let model = &provider["models"][0];
                assert_eq!(model["id"], "agent-model");
                assert_eq!(model["reasoning"], false);
                assert_eq!(model["input"], serde_json::json!(["text"]));
                assert_eq!(model["contextWindow"], 128000);
                assert_eq!(model["maxTokens"], 16384);
                assert_eq!(
                    model["cost"],
                    serde_json::json!({
                        "input": 0,
                        "output": 0,
                        "cacheRead": 0,
                        "cacheWrite": 0,
                    })
                );
                assert!(model.get("api").is_none());
                assert!(model.get("provider").is_none());
                assert!(model.get("baseUrl").is_none());
                assert!(value.get("defaultModel").is_none());
            }
            "qwen-code" => {
                let value: serde_json::Value = serde_json::from_str(&overlay.content).unwrap();
                let model = &value["modelProviders"]["openai"][0];
                assert_eq!(model["id"], "agent-model");
                assert_eq!(model["name"], "agent-model");
                assert_eq!(model["baseUrl"], ENDPOINT);
                assert_eq!(model["envKey"], "VIBEX_QWEN_CODE_API_KEY");
                assert_eq!(value["security"]["auth"]["selectedType"], "openai");
                assert_eq!(value["model"]["name"], "agent-model");
                assert!(model.get("model").is_none());
                assert!(value["security"]["auth"].get("apiKey").is_none());
            }
            "stakpak" => {
                assert!(!overlay.contains_secret_reference);
                let value: toml::Value = toml::from_str(&overlay.content).unwrap();
                let profile = &value["profiles"]["vibex"];
                assert_eq!(profile["provider"].as_str(), Some("local"));
                assert_eq!(profile["model"].as_str(), Some("openai/agent-model"));
                assert_eq!(
                    profile["providers"]["openai"]["type"].as_str(),
                    Some("openai")
                );
                assert_eq!(
                    profile["providers"]["openai"]["api_endpoint"].as_str(),
                    Some(ENDPOINT)
                );
                assert!(profile["providers"]["openai"].get("api_key").is_none());
            }
            "vtcode" => {
                let value: toml::Value = toml::from_str(&overlay.content).unwrap();
                assert_eq!(
                    value["agent"]["provider"].as_str(),
                    Some("vibex-matrix-provider")
                );
                assert_eq!(
                    value["agent"]["default_model"].as_str(),
                    Some("agent-model")
                );
                let provider = &value["custom_providers"][0];
                assert_eq!(provider["name"].as_str(), Some("vibex-matrix-provider"));
                assert_eq!(provider["base_url"].as_str(), Some(ENDPOINT));
                assert_eq!(provider["model"].as_str(), Some("agent-model"));
                assert_eq!(
                    provider["api_key_env"].as_str(),
                    Some("VIBEX_VTCODE_API_KEY")
                );
            }
            _ => {}
        }
    }

    #[test]
    fn kimi_projection_materializes_a_private_provider_config_and_auth_compatibility_token() {
        let descriptors = vibex_core::catalog_projection_descriptors().unwrap();
        let descriptor = descriptors
            .iter()
            .find(|descriptor| descriptor.route.agent_id.as_str() == "kimi")
            .unwrap();
        let (provider, runtime, binding) = typed_projection_fixture(descriptor);
        let mut plan = AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            descriptor,
            "kimi-projection",
        )
        .unwrap();
        plan.secret_env[0].secret_reference.backend = ProviderSecretBackend::OsKeychain;
        plan.secret_env[0].secret_reference.setup_state = ProviderSecretSetupState::Available;
        assert_eq!(plan.overlay_files.len(), 2);
        assert_eq!(plan.overlay_files[0].relative_path, "config.toml");
        assert_eq!(
            plan.overlay_files[1].relative_path,
            "credentials/kimi-code.json"
        );
        let temp = tempdir().unwrap();
        let secret = plan.secret_env[0].secret_reference.clone();
        let lookup_key = secret.lookup_key.clone();
        secrets::store_provider_secret(&lookup_key, TYPED_SECRET_SENTINEL).unwrap();
        let resolved = AgentProviderProjectionEngine::resolve_and_materialize(
            &plan,
            temp.path(),
            "kimi-projection",
        )
        .unwrap();
        let config = fs::read_to_string(&resolved.overlay_files[0]).unwrap();
        let config: toml::Value = toml::from_str(&config).unwrap();
        assert_eq!(config["default_model"].as_str(), Some("agent-model"));
        assert_eq!(
            config["providers"]["vibex"]["type"].as_str(),
            Some("openai_legacy")
        );
        assert_eq!(
            config["providers"]["vibex"]["base_url"].as_str(),
            Some("https://provider.example.invalid/v1/")
        );
        assert_eq!(
            config["providers"]["vibex"]["api_key"].as_str(),
            Some(TYPED_SECRET_SENTINEL)
        );
        let auth = fs::read_to_string(&resolved.overlay_files[1]).unwrap();
        let auth: serde_json::Value = serde_json::from_str(&auth).unwrap();
        assert_eq!(auth["access_token"], TYPED_SECRET_SENTINEL);
        assert_eq!(auth["expires_at"], 0);
        assert_eq!(
            resolved
                .non_secret_env
                .get("KIMI_SHARE_DIR")
                .map(PathBuf::from),
            Some(resolved.overlay_root.clone())
        );
        secrets::delete_provider_secret(&lookup_key).unwrap();
    }

    #[test]
    fn all_typed_catalog_projectors_map_provider_env_secret_model_and_private_state() {
        let descriptors = vibex_core::catalog_projection_descriptors().unwrap();
        let expectations = typed_projection_expectations();
        assert_eq!(expectations.len(), 18);

        for expected in expectations {
            let descriptor = descriptors
                .iter()
                .find(|descriptor| descriptor.route.agent_id.as_str() == expected.agent_id)
                .unwrap_or_else(|| panic!("missing descriptor for {}", expected.agent_id));
            assert_eq!(
                descriptor.evidence.state,
                ProjectionEvidenceState::Documented
            );
            assert_eq!(
                descriptor.runtime_home_strategy,
                AgentRuntimeHomeStrategy::VibexPrivate
            );
            assert_eq!(
                descriptor.switch_behavior,
                ProviderSwitchBehavior::RestartAndResume
            );

            let descriptor_secret_key = match &descriptor.credential_control {
                AgentCredentialControl::Environment { secret_env_key, .. } => {
                    secret_env_key.as_str()
                }
                control => panic!(
                    "{} has unexpected credential control {control:?}",
                    expected.agent_id
                ),
            };
            assert_eq!(descriptor_secret_key, expected.secret_env_key);

            match (&descriptor.provider_control, expected.base_url_key) {
                (AgentProviderControl::Environment { base_url_key }, Some(expected_key)) => {
                    assert_eq!(base_url_key.as_deref(), Some(expected_key))
                }
                (AgentProviderControl::ManagedConfigOverlay { .. }, None) => {}
                (control, expected_key) => panic!(
                    "{} provider control {control:?} does not match base URL expectation {expected_key:?}",
                    expected.agent_id
                ),
            }
            match (&descriptor.model_control, expected.model_env_key) {
                (AgentModelControl::ProcessEnvironment { key }, Some(expected_key)) => {
                    assert_eq!(key, expected_key)
                }
                (AgentModelControl::ManagedConfigOverlay { .. }, None) => {}
                (AgentModelControl::AcpConfigOption { aliases }, None)
                    if expected.agent_id == "antigravity" =>
                {
                    assert_eq!(aliases, &["model"])
                }
                (control, expected_key) => panic!(
                    "{} model control {control:?} does not match model env expectation {expected_key:?}",
                    expected.agent_id
                ),
            }

            let (provider, runtime, binding) = typed_projection_fixture(descriptor);
            let plan = AgentProviderProjectionEngine::plan(
                &provider,
                &runtime,
                &binding,
                descriptor,
                "typed-matrix",
            )
            .unwrap_or_else(|error| panic!("{} projection failed: {error:?}", expected.agent_id));
            assert_eq!(plan.secret_env.len(), 1, "{}", expected.agent_id);
            assert_eq!(plan.secret_env[0].key, expected.secret_env_key);
            let expected_model = match expected.agent_id {
                "pi" => "matrix-provider/agent-model",
                "hermes" => "custom:agent-model",
                "antigravity" => "agent-model-high",
                _ => "agent-model",
            };
            assert_eq!(plan.effective_model.as_deref(), Some(expected_model));
            assert_eq!(
                plan.overlay_files.len(),
                if expected.agent_id == "kimi" {
                    2
                } else if expected.overlay_path.is_some() {
                    1
                } else {
                    0
                }
            );
            if let Some((path, format)) = expected.overlay_path.zip(expected.overlay_format) {
                let overlay = &plan.overlay_files[0];
                assert_eq!(overlay.relative_path, path, "{}", expected.agent_id);
                assert_eq!(overlay.format, format, "{}", expected.agent_id);
                assert_overlay_schema(expected.agent_id, overlay);
                assert_typed_overlay_contract(expected.agent_id, overlay);
            }

            let rendered = format!("{plan:?}");
            assert!(!rendered.contains(TYPED_SECRET_LOOKUP));
            assert!(!rendered.contains(TYPED_SECRET_SENTINEL));
            let preview = serde_json::to_string(&plan.preview).unwrap();
            assert!(!preview.contains(TYPED_SECRET_LOOKUP));
            assert!(!preview.contains(TYPED_SECRET_SENTINEL));

            let runtime_root = tempdir().unwrap();
            let non_secret_env = AgentProviderProjectionEngine::non_secret_environment_for_plan(
                &plan,
                runtime_root.path(),
                "typed-matrix",
            )
            .unwrap();
            if let Some(home_key) = expected.runtime_home_env_key {
                let home = non_secret_env.get(home_key).unwrap();
                assert!(home.starts_with(runtime_root.path().to_string_lossy().as_ref()));
                if expected.agent_id == "vtcode" {
                    assert!(home.ends_with("vtcode.toml"));
                }
            }
            if let Some(base_url_key) = expected.base_url_key {
                let base_url = non_secret_env.get(base_url_key).unwrap();
                if matches!(expected.agent_id, "antigravity" | "gemini" | "poolside") {
                    assert_eq!(base_url, "https://provider.example.invalid");
                } else {
                    assert_eq!(base_url, "https://provider.example.invalid/v1/");
                }
            }
            if let Some(model_key) = expected.model_env_key {
                let model = non_secret_env.get(model_key).unwrap();
                assert_eq!(model, "agent-model");
            }
            match expected.agent_id {
                "codewhale" => assert_eq!(
                    non_secret_env.get("CODEWHALE_PROVIDER").map(String::as_str),
                    Some("openai")
                ),
                "goose" => {
                    assert_eq!(
                        non_secret_env.get("GOOSE_PROVIDER").map(String::as_str),
                        Some("matrix-provider")
                    );
                    assert_eq!(
                        non_secret_env.get("GOOSE_MODEL").map(String::as_str),
                        Some("agent-model")
                    );
                }
                _ => {}
            }
            if expected.agent_id == "kilo" {
                let inline = non_secret_env.get("KILO_CONFIG_CONTENT").unwrap();
                let value: serde_json::Value = serde_json::from_str(inline).unwrap();
                assert!(value.get("provider").is_some());
                assert_eq!(inline, &plan.overlay_files[0].content);
                assert!(!inline.contains(TYPED_SECRET_LOOKUP));
            }

            let process_args = AgentProviderProjectionEngine::process_args_for_plan(
                &plan,
                runtime_root.path(),
                "typed-matrix",
            )
            .unwrap();
            match expected.agent_id {
                "crow-cli" => {
                    assert_eq!(
                        process_args.first().map(String::as_str),
                        Some("--config-dir")
                    );
                    assert!(
                        process_args[1].starts_with(runtime_root.path().to_string_lossy().as_ref())
                    );
                }
                "stakpak" => {
                    assert_eq!(process_args.len(), 4);
                    assert_eq!(process_args[0], "--profile");
                    assert_eq!(process_args[1], "vibex");
                    assert_eq!(process_args[2], "--config");
                    assert!(process_args[3].ends_with("/stakpak.toml"));
                }
                _ => assert!(process_args.is_empty(), "{}", expected.agent_id),
            }

            let mut changed_provider = provider.clone();
            if let AgentCredential::ApiKey { secret, .. } =
                &mut changed_provider.credentials[0].credential
            {
                secret.revision += 1;
            }
            let changed = AgentProviderProjectionEngine::plan(
                &changed_provider,
                &runtime,
                &binding,
                descriptor,
                "typed-matrix",
            )
            .unwrap();
            assert_ne!(
                plan.fingerprint, changed.fingerprint,
                "{}",
                expected.agent_id
            );
            assert!(plan.fingerprint.starts_with("sha256:"));
        }
    }

    #[test]
    fn claude_projection_preserves_declared_anthropic_credential_environment() {
        let descriptor = vibex_core::AgentProviderProjectionRegistry::builtin()
            .unwrap()
            .descriptors()
            .find(|descriptor| descriptor.route.agent_id.as_str() == CLAUDE_AGENT_ID)
            .cloned()
            .expect("builtin Claude projection descriptor");
        let (mut provider, runtime, binding) = typed_projection_fixture(&descriptor);

        for (display_name, expected_key) in [
            (CLAUDE_AUTH_TOKEN_ENV, CLAUDE_AUTH_TOKEN_ENV),
            (CLAUDE_API_KEY_ENV, CLAUDE_API_KEY_ENV),
            ("Claude API credential", CLAUDE_API_KEY_ENV),
        ] {
            provider.credentials[0].display_name = display_name.to_string();
            let plan = AgentProviderProjectionEngine::plan(
                &provider,
                &runtime,
                &binding,
                &descriptor,
                "claude-credential-environment",
            )
            .unwrap();
            assert_eq!(plan.secret_env[0].key, expected_key);
            assert!(
                plan.preview
                    .targets
                    .iter()
                    .any(|target| target.field == "credential" && target.target == expected_key)
            );
        }
    }

    #[test]
    fn structured_overlay_is_deterministic_private_and_redacted() {
        let (provider, runtime, binding, descriptor) =
            fixture(ConfigOverlayStrategy::StructuredJsonOverlay);
        let first = AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            &descriptor,
            "workspace-test",
        )
        .unwrap();
        let second = AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            &descriptor,
            "workspace-test",
        )
        .unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        let encoded = serde_json::to_string(&first.preview).unwrap();
        assert!(!encoded.contains("never-preview"));
        assert!(!encoded.contains("/private/user"));

        let dir = tempdir().unwrap();
        let resolved = AgentProviderProjectionEngine::resolve_and_materialize(
            &first,
            dir.path(),
            "workspace-test",
        )
        .unwrap();
        assert_eq!(resolved.overlay_files.len(), 1);
        assert!(resolved.overlay_files[0].starts_with(dir.path()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&resolved.overlay_files[0])
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn opencode_overlay_omits_absent_model_display_name() {
        let (mut provider, _, binding, _) = fixture(ConfigOverlayStrategy::OpenCodeInlineProvider);
        let endpoint = provider.endpoints.first().unwrap();

        let content = opencode_overlay(&provider, &binding, Some(endpoint)).unwrap();
        let overlay: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            overlay["provider"]["fake"]["models"]["model-a"],
            serde_json::json!({})
        );

        provider.configured_models[0].display_name = Some("Model A".to_string());
        let content = opencode_overlay(&provider, &binding, Some(endpoint)).unwrap();
        let overlay: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            overlay["provider"]["fake"]["models"]["model-a"]["name"],
            "Model A"
        );
    }

    #[test]
    fn opencode_overlay_projects_declared_model_capabilities() {
        let (mut provider, _, binding, _) = fixture(ConfigOverlayStrategy::OpenCodeInlineProvider);
        provider.configured_models[0].capabilities = ProviderModelCapabilities {
            reasoning: Some(true),
            temperature: Some(false),
            image_input: Some(true),
            pdf_input: Some(true),
            context_tokens: Some(1_000_000),
            output_tokens: Some(128_000),
        };
        let endpoint = provider.endpoints.first().unwrap();

        let content = opencode_overlay(&provider, &binding, Some(endpoint)).unwrap();
        let overlay: serde_json::Value = serde_json::from_str(&content).unwrap();
        let model = &overlay["provider"]["fake"]["models"]["model-a"];

        // Reasoning is what unlocks OpenCode's run-mode variants; Vibex declares
        // the capability and lets OpenCode derive the effort levels.
        assert_eq!(model["reasoning"], serde_json::json!(true));
        assert_eq!(model["temperature"], serde_json::json!(false));
        assert_eq!(model["attachment"], serde_json::json!(true));
        assert_eq!(
            model["modalities"],
            serde_json::json!({ "input": ["text", "image", "pdf"], "output": ["text"] })
        );
        assert_eq!(
            model["limit"],
            serde_json::json!({ "context": 1_000_000, "output": 128_000 })
        );
        assert!(
            model.get("variants").is_none(),
            "variants are derived by OpenCode, never projected by Vibex"
        );
    }

    #[test]
    fn opencode_overlay_projects_image_input_without_pdf() {
        let (mut provider, _, binding, _) = fixture(ConfigOverlayStrategy::OpenCodeInlineProvider);
        provider.configured_models[0].capabilities = ProviderModelCapabilities {
            image_input: Some(true),
            ..ProviderModelCapabilities::default()
        };
        let endpoint = provider.endpoints.first().unwrap();

        let content = opencode_overlay(&provider, &binding, Some(endpoint)).unwrap();
        let overlay: serde_json::Value = serde_json::from_str(&content).unwrap();
        let model = &overlay["provider"]["fake"]["models"]["model-a"];

        assert_eq!(model["attachment"], serde_json::json!(true));
        assert_eq!(
            model["modalities"]["input"],
            serde_json::json!(["text", "image"])
        );
        // Undeclared capabilities stay absent rather than being guessed.
        assert!(model.get("reasoning").is_none());
        assert!(model.get("limit").is_none());
    }

    #[test]
    fn opencode_overlay_omits_partial_token_limits() {
        let (mut provider, _, binding, _) = fixture(ConfigOverlayStrategy::OpenCodeInlineProvider);
        provider.configured_models[0].capabilities = ProviderModelCapabilities {
            context_tokens: Some(1_000_000),
            ..ProviderModelCapabilities::default()
        };
        let endpoint = provider.endpoints.first().unwrap();

        let content = opencode_overlay(&provider, &binding, Some(endpoint)).unwrap();
        let overlay: serde_json::Value = serde_json::from_str(&content).unwrap();

        // OpenCode needs both bounds; a half-declared limit is worse than none.
        assert_eq!(
            overlay["provider"]["fake"]["models"]["model-a"],
            serde_json::json!({})
        );
    }

    #[test]
    fn opencode_overlay_projects_explicitly_disabled_capabilities() {
        let (mut provider, _, binding, _) = fixture(ConfigOverlayStrategy::OpenCodeInlineProvider);
        provider.configured_models[0].capabilities = ProviderModelCapabilities {
            reasoning: Some(false),
            image_input: Some(false),
            ..ProviderModelCapabilities::default()
        };
        let endpoint = provider.endpoints.first().unwrap();

        let content = opencode_overlay(&provider, &binding, Some(endpoint)).unwrap();
        let overlay: serde_json::Value = serde_json::from_str(&content).unwrap();
        let model = &overlay["provider"]["fake"]["models"]["model-a"];

        // A declared "false" is a real statement and must survive the round trip.
        assert_eq!(model["reasoning"], serde_json::json!(false));
        assert_eq!(model["attachment"], serde_json::json!(false));
        assert_eq!(model["modalities"]["input"], serde_json::json!(["text"]));
    }

    #[test]
    fn opencode_anthropic_base_url_targets_the_versioned_api_root() {
        for (endpoint, expected) in [
            (
                "https://agentrouter.example",
                "https://agentrouter.example/v1",
            ),
            (
                "https://agentrouter.example/anthropic/",
                "https://agentrouter.example/anthropic/v1",
            ),
            (
                "https://agentrouter.example/v1/",
                "https://agentrouter.example/v1",
            ),
            (
                "https://agentrouter.example/v1/messages",
                "https://agentrouter.example/v1",
            ),
        ] {
            assert_eq!(
                opencode_base_url_for_model(endpoint, vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES),
                expected
            );
        }
        assert_eq!(
            opencode_base_url_for_model(
                "https://agentrouter.example",
                vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES
            ),
            "https://agentrouter.example"
        );
    }

    #[test]
    fn opencode_projection_keeps_prequalified_model_ids_idempotent() {
        let (provider, _, mut binding, descriptor) =
            fixture(ConfigOverlayStrategy::OpenCodeInlineProvider);
        binding.configured_models[0].provider_model_id = "fake/model-a".to_string();
        binding.configured_models[0].agent_model_id = "fake/model-a".to_string();
        let endpoint = provider.endpoints.first().unwrap();

        let content = opencode_overlay(&provider, &binding, Some(endpoint)).unwrap();
        let overlay: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            overlay["provider"]["fake"]["models"]
                .get("model-a")
                .is_some()
        );
        assert!(
            overlay["provider"]["fake"]["models"]
                .get("fake/model-a")
                .is_none()
        );
        assert_eq!(
            projected_runtime_model_id(&provider, &descriptor, &binding.configured_models[0]),
            "fake/model-a"
        );
    }

    #[test]
    fn pi_projection_namespaces_models_by_overlay_provider() {
        let (provider, _, binding, descriptor) = fixture(ConfigOverlayStrategy::PiModelsJson);

        assert_eq!(
            projected_runtime_model_id(&provider, &descriptor, &binding.configured_models[0]),
            "fake/model-a"
        );

        let mut prequalified = binding.configured_models[0].clone();
        prequalified.agent_model_id = "fake/model-a".to_string();
        assert_eq!(
            projected_runtime_model_id(&provider, &descriptor, &prequalified),
            "fake/model-a"
        );
    }

    #[test]
    fn pi_overlay_projects_declared_reasoning_capability() {
        let (mut provider, _, binding, _) = fixture(ConfigOverlayStrategy::PiModelsJson);
        let endpoint = provider.endpoints.first().cloned().unwrap();

        let content = pi_overlay(
            &provider,
            Some(&endpoint),
            binding.configured_models.first(),
            "PI_API_KEY",
        )
        .unwrap();
        let overlay: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            overlay["providers"]["fake"]["models"][0]["reasoning"],
            false
        );

        provider.configured_models[0].capabilities.reasoning = Some(true);
        let content = pi_overlay(
            &provider,
            Some(&endpoint),
            binding.configured_models.first(),
            "PI_API_KEY",
        )
        .unwrap();
        let overlay: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(overlay["providers"]["fake"]["models"][0]["reasoning"], true);
    }

    #[test]
    fn opencode_overlay_maps_all_five_protocols_to_their_sdk_adapters() {
        let (mut provider, _, mut binding, descriptor) =
            fixture(ConfigOverlayStrategy::OpenCodeInlineProvider);
        provider.endpoints.extend([
            ModelProviderEndpoint {
                id: "anthropic".to_string(),
                kind: ModelProviderEndpointKind::Api,
                url: "https://anthropic.example.invalid".to_string(),
                wire_protocol_id: Some(vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string()),
            },
            ModelProviderEndpoint {
                id: "google".to_string(),
                kind: ModelProviderEndpointKind::Api,
                url: "https://google.example.invalid/v1beta".to_string(),
                wire_protocol_id: Some(vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI.to_string()),
            },
            ModelProviderEndpoint {
                id: "bedrock".to_string(),
                kind: ModelProviderEndpointKind::Api,
                url: "https://bedrock.example.invalid".to_string(),
                wire_protocol_id: Some(vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE.to_string()),
            },
        ]);
        let credential_id = RequestId::new();
        provider.credentials = vec![ModelProviderCredentialReference {
            id: credential_id.clone(),
            display_name: "OpenCode API key".to_string(),
            status: AgentCredentialStatus::Referenced,
            credential: AgentCredential::ApiKey {
                secret: ProjectionSecretReference {
                    id: credential_id,
                    backend: ProviderSecretBackend::Placeholder,
                    setup_state: ProviderSecretSetupState::Missing,
                    lookup_key: "opencode-test-secret".to_string(),
                    redacted_hint: "configured".to_string(),
                    revision: 1,
                    legacy_secret_reference_id: None,
                },
                target_hint: None,
            },
            revision: 1,
        }];
        binding.configured_models = [
            (
                "responses",
                vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES,
                "@ai-sdk/openai",
            ),
            (
                "chat",
                vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                "@ai-sdk/openai-compatible",
            ),
            (
                "anthropic",
                vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
                "@ai-sdk/anthropic",
            ),
            (
                "google",
                vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI,
                "@ai-sdk/google",
            ),
            (
                "bedrock",
                vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE,
                "@ai-sdk/amazon-bedrock",
            ),
        ]
        .into_iter()
        .map(|(model, protocol, sdk)| AgentConfiguredModelBinding {
            id: AgentConfiguredModelBindingId::new(),
            provider_model_id: model.to_string(),
            agent_model_id: model.to_string(),
            wire_protocol_id: protocol.to_string(),
            sdk_adapter_id: Some(sdk.to_string()),
            deployment: None,
            enabled: true,
            process_scoped: true,
        })
        .collect();

        let endpoint = provider.endpoints.first().unwrap();
        let content = opencode_overlay(&provider, &binding, Some(endpoint)).unwrap();
        let overlay: serde_json::Value = serde_json::from_str(&content).unwrap();
        let providers = overlay["provider"].as_object().unwrap();
        for (provider_id, npm) in [
            ("fake", "@ai-sdk/openai"),
            ("fake-chat", "@ai-sdk/openai-compatible"),
            ("fake-anthropic", "@ai-sdk/anthropic"),
            ("fake-google", "@ai-sdk/google"),
            ("fake-bedrock", "@ai-sdk/amazon-bedrock"),
        ] {
            assert_eq!(providers[provider_id]["npm"], npm);
        }
        assert_eq!(
            providers["fake-bedrock"]["options"]["headers"]["Authorization"],
            "Bearer {env:VIBEX_OPENCODE_PROVIDER_API_KEY}"
        );
        assert!(providers["fake-bedrock"]["options"].get("apiKey").is_none());
        assert_eq!(
            providers["fake-google"]["options"]["apiKey"],
            "{env:VIBEX_OPENCODE_PROVIDER_API_KEY}"
        );
        assert_eq!(
            providers["fake-google"]["options"]["baseURL"],
            "https://google.example.invalid/v1beta"
        );
        assert_eq!(
            providers["fake-anthropic"]["options"]["baseURL"],
            "https://anthropic.example.invalid/v1"
        );
        assert_eq!(
            providers["fake-bedrock"]["options"]["baseURL"],
            "https://bedrock.example.invalid"
        );
        assert_eq!(providers["fake-chat"]["options"]["baseURL"], endpoint.url);
        for (model_id, runtime_model_id) in [
            ("responses", "fake/responses"),
            ("chat", "fake-chat/chat"),
            ("anthropic", "fake-anthropic/anthropic"),
            ("google", "fake-google/google"),
            ("bedrock", "fake-bedrock/bedrock"),
        ] {
            let model = binding
                .configured_models
                .iter()
                .find(|model| model.provider_model_id == model_id)
                .unwrap();
            assert_eq!(
                projected_runtime_model_id(&provider, &descriptor, model),
                runtime_model_id
            );
        }
    }

    #[test]
    fn antigravity_uses_high_as_the_initial_runtime_variant() {
        let descriptor = vibex_core::catalog_projection_descriptors()
            .unwrap()
            .into_iter()
            .find(|descriptor| descriptor.route.agent_id.as_str() == "antigravity")
            .unwrap();
        let (provider, _, binding) = typed_projection_fixture(&descriptor);
        let mut model = binding.configured_models[0].clone();

        model.agent_model_id = "gemini-3.7-flash".to_string();
        assert_eq!(
            projected_runtime_model_id(&provider, &descriptor, &model),
            "gemini-3.7-flash-high"
        );

        model.agent_model_id = "gemini-3.7-flash-medium".to_string();
        assert_eq!(
            projected_runtime_model_id(&provider, &descriptor, &model),
            "gemini-3.7-flash-medium"
        );
    }

    #[test]
    fn hermes_protocol_modes_are_exact_and_secret_is_materialized_only_privately() {
        for (protocol, expected_mode) in [
            (
                vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
                "chat_completions",
            ),
            (
                vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
                "anthropic_messages",
            ),
            (
                vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES,
                "codex_responses",
            ),
            (
                vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE,
                "bedrock_converse",
            ),
        ] {
            let model = AgentConfiguredModelBinding {
                id: AgentConfiguredModelBindingId::new(),
                provider_model_id: "provider-model".to_string(),
                agent_model_id: "agent-model".to_string(),
                wire_protocol_id: protocol.to_string(),
                sdk_adapter_id: None,
                deployment: None,
                enabled: true,
                process_scoped: true,
            };
            assert_eq!(hermes_api_mode(Some(&model)), expected_mode);
        }

        let descriptor = vibex_core::catalog_projection_descriptors()
            .unwrap()
            .into_iter()
            .find(|descriptor| descriptor.route.agent_id.as_str() == "hermes")
            .unwrap();
        let (mut provider, runtime, binding) = typed_projection_fixture(&descriptor);
        assert_eq!(
            projected_runtime_model_id(&provider, &descriptor, &binding.configured_models[0]),
            "custom:agent-model"
        );
        let mut prequalified = binding.configured_models[0].clone();
        prequalified.agent_model_id = "custom:agent-model".to_string();
        assert_eq!(
            projected_runtime_model_id(&provider, &descriptor, &prequalified),
            "custom:agent-model"
        );
        let lookup_key = format!("hermes-materialize-{}", RequestId::new());
        let secret_value = "hermes-private-secret";
        let AgentCredential::ApiKey { secret, .. } = &mut provider.credentials[0].credential else {
            panic!("Hermes fixture must use an API key")
        };
        secret.backend = ProviderSecretBackend::OsKeychain;
        secret.setup_state = ProviderSecretSetupState::Available;
        secret.lookup_key = lookup_key.clone();

        let plan = AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            &descriptor,
            "hermes-secret",
        )
        .unwrap();
        assert!(plan.overlay_files[0].contains_secret_reference);
        assert!(!plan.overlay_files[0].content.contains(secret_value));
        assert!(!format!("{plan:?}").contains(secret_value));

        secrets::store_provider_secret(&lookup_key, secret_value).unwrap();
        let runtime_root = tempdir().unwrap();
        let resolved = AgentProviderProjectionEngine::resolve_and_materialize(
            &plan,
            runtime_root.path(),
            "hermes-secret",
        )
        .unwrap();
        let materialized = fs::read_to_string(&resolved.overlay_files[0]).unwrap();
        let yaml: serde_yaml::Value = serde_yaml::from_str(&materialized).unwrap();
        assert_eq!(yaml["custom_providers"][0]["api_key"], secret_value);
        assert!(!materialized.contains(OVERLAY_SECRET_PLACEHOLDER_PREFIX));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&resolved.overlay_files[0])
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        secrets::delete_provider_secret(&lookup_key).unwrap();
    }

    #[test]
    fn selected_endpoint_prefers_protocol_match_and_rejects_override_mismatch() {
        let (mut provider, _, mut binding, _) =
            fixture(ConfigOverlayStrategy::StructuredJsonOverlay);
        provider.endpoints = vec![
            ModelProviderEndpoint {
                id: "fallback".to_string(),
                kind: ModelProviderEndpointKind::Api,
                url: "https://fallback.example.invalid/v1".to_string(),
                wire_protocol_id: None,
            },
            ModelProviderEndpoint {
                id: "google".to_string(),
                kind: ModelProviderEndpointKind::Api,
                url: "https://google.example.invalid/v1beta".to_string(),
                wire_protocol_id: Some(vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI.to_string()),
            },
        ];
        binding.configured_models[0].wire_protocol_id =
            vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI.to_string();
        let model = binding.configured_models.first().unwrap();
        assert_eq!(
            selected_endpoint(&provider, &binding, Some(model))
                .unwrap()
                .map(|endpoint| endpoint.id.as_str()),
            Some("google")
        );

        binding.projection_overrides.endpoint_id = Some("fallback".to_string());
        assert_eq!(
            selected_endpoint(&provider, &binding, Some(model))
                .unwrap()
                .map(|endpoint| endpoint.id.as_str()),
            Some("fallback")
        );
        binding.projection_overrides.endpoint_id = Some("google".to_string());
        binding.configured_models[0].wire_protocol_id =
            vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE.to_string();
        assert_eq!(
            selected_endpoint(&provider, &binding, binding.configured_models.first())
                .unwrap_err()
                .code,
            "agent_projection_endpoint_protocol_mismatch"
        );
    }

    #[test]
    fn exact_conservative_evidence_never_promotes_a_binding_to_ready() {
        let (_, _, mut binding, mut descriptor) =
            fixture(ConfigOverlayStrategy::StructuredJsonOverlay);

        descriptor.evidence.state = ProjectionEvidenceState::Documented;
        assert_eq!(
            status_for_resolution(ProjectionDescriptorMatch::Exact, &descriptor),
            AgentModelProviderBindingStatus::Unverified
        );
        binding.verification = verification_from_descriptor(&descriptor);
        binding.projection_fingerprint = Some("sha256:active".to_string());
        apply_stale_state(
            &mut binding,
            "sha256:next",
            ProjectionDescriptorMatch::Exact,
        );
        assert_eq!(binding.status, AgentModelProviderBindingStatus::Unverified);
        assert_eq!(
            binding.verification.state,
            ProjectionEvidenceState::Documented
        );
        assert!(binding.projection_fingerprint.is_none());

        descriptor.evidence.state = ProjectionEvidenceState::Unverified;
        assert_eq!(
            status_for_resolution(ProjectionDescriptorMatch::Exact, &descriptor),
            AgentModelProviderBindingStatus::Unverified
        );
        binding.verification = verification_from_descriptor(&descriptor);
        binding.projection_fingerprint = Some("sha256:active".to_string());
        apply_stale_state(
            &mut binding,
            "sha256:next",
            ProjectionDescriptorMatch::Exact,
        );
        assert_eq!(binding.status, AgentModelProviderBindingStatus::Unverified);
        assert!(binding.projection_fingerprint.is_none());

        descriptor.evidence.state = ProjectionEvidenceState::Unsupported;
        assert_eq!(
            status_for_resolution(ProjectionDescriptorMatch::Exact, &descriptor),
            AgentModelProviderBindingStatus::Unsupported
        );
        binding.verification = verification_from_descriptor(&descriptor);
        binding.projection_fingerprint = Some("sha256:active".to_string());
        apply_stale_state(
            &mut binding,
            "sha256:next",
            ProjectionDescriptorMatch::Exact,
        );
        assert_eq!(binding.status, AgentModelProviderBindingStatus::Unsupported);
        assert!(binding.projection_fingerprint.is_none());
    }

    #[test]
    fn overlay_rejects_parent_escape_and_symlink() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("runtime");
        ensure_private_directory(&root).unwrap();
        assert_eq!(
            safe_overlay_path(&root, "../outside.json")
                .unwrap_err()
                .code,
            "agent_projection_overlay_path_unsafe"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("outside"), root.join("config.toml"))
                .unwrap();
            assert_eq!(
                safe_overlay_path(&root, "config.toml").unwrap_err().code,
                "agent_projection_overlay_symlink_rejected"
            );
        }
    }

    #[test]
    fn process_scoped_changes_affect_fingerprint_but_workspace_key_does_not() {
        let (provider, runtime, mut binding, descriptor) =
            fixture(ConfigOverlayStrategy::StructuredTomlOverlay);
        let first = AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            &descriptor,
            "workspace-a",
        )
        .unwrap();
        let other_workspace = AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            &descriptor,
            "workspace-b",
        )
        .unwrap();
        assert_eq!(first.fingerprint, other_workspace.fingerprint);
        binding.configured_models[0].agent_model_id = "model-b".to_string();
        let changed = AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            &descriptor,
            "workspace-a",
        )
        .unwrap();
        assert_ne!(first.fingerprint, changed.fingerprint);
    }

    #[test]
    fn secret_resolution_is_late_and_debug_preview_remain_redacted() {
        let (mut provider, runtime, binding, mut descriptor) =
            fixture(ConfigOverlayStrategy::StructuredJsonOverlay);
        let secret_value = "projection-secret-must-not-leak";
        let lookup_key = format!("projection-test-{}", RequestId::new());
        let credential_id = RequestId::new();
        provider.credentials = vec![ModelProviderCredentialReference {
            id: credential_id.clone(),
            display_name: "Test API key".to_string(),
            status: AgentCredentialStatus::Ready,
            credential: AgentCredential::ApiKey {
                secret: vibex_core::ProjectionSecretReference {
                    id: credential_id,
                    backend: ProviderSecretBackend::OsKeychain,
                    setup_state: ProviderSecretSetupState::Available,
                    lookup_key: lookup_key.clone(),
                    redacted_hint: "configured".to_string(),
                    revision: 1,
                    legacy_secret_reference_id: None,
                },
                target_hint: Some("FAKE_API_KEY".to_string()),
            },
            revision: 1,
        }];
        descriptor.credential_control = AgentCredentialControl::Environment {
            secret_env_key: "FAKE_API_KEY".to_string(),
            accepted_secret_kinds: vec![ProviderSecretKind::ApiKey],
        };
        descriptor.credential_kinds = vec![AgentCredentialKind::ApiKey];

        let plan = AgentProviderProjectionEngine::plan(
            &provider,
            &runtime,
            &binding,
            &descriptor,
            "workspace-late-secret",
        )
        .unwrap();
        assert_eq!(plan.secret_env.len(), 1);
        assert!(!format!("{plan:?}").contains(secret_value));
        assert!(
            !serde_json::to_string(&plan.preview)
                .unwrap()
                .contains(&lookup_key)
        );

        secrets::store_provider_secret(&lookup_key, secret_value).unwrap();
        let dir = tempdir().unwrap();
        let resolved = AgentProviderProjectionEngine::resolve_and_materialize(
            &plan,
            dir.path(),
            "workspace-late-secret",
        )
        .unwrap();
        assert_eq!(resolved.secret_env[0].value.expose(), secret_value);
        assert!(!format!("{resolved:?}").contains(secret_value));
        assert!(!format!("{:?}", resolved.secret_env).contains(secret_value));
        secrets::delete_provider_secret(&lookup_key).unwrap();
    }

    #[test]
    fn shared_provider_endpoint_update_marks_only_affected_binding_stale() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let provider = service
            .create_model_provider_profile(ModelProviderProfileCreateRequest {
                display_name: "Shared gateway".to_string(),
                vendor_hint: Some("shared".to_string()),
                endpoints: vec![
                    ModelProviderEndpoint {
                        id: "claude-api".to_string(),
                        kind: ModelProviderEndpointKind::Api,
                        url: "https://claude.example.invalid/v1".to_string(),
                        wire_protocol_id: Some(
                            vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string(),
                        ),
                    },
                    ModelProviderEndpoint {
                        id: "codex-api".to_string(),
                        kind: ModelProviderEndpointKind::Api,
                        url: "https://codex.example.invalid/v1".to_string(),
                        wire_protocol_id: Some(
                            vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES.to_string(),
                        ),
                    },
                ],
                proxy_policy: ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: Vec::new(),
                default_model_id: None,
                headers: Vec::new(),
                status: ModelProviderProfileStatus::Enabled,
            })
            .unwrap();

        let runtime_request = |agent_id: &str, adapter_id: &str, adapter_version: &str| {
            let mut runtime_dependencies = BTreeMap::new();
            let agent_version = if agent_id == "codex" {
                runtime_dependencies.insert("@openai/codex".to_string(), "0.146.0".to_string());
                Some("0.146.0".to_string())
            } else {
                None
            };
            AgentRuntimeProfileCreateRequest {
                version_identity: AgentRuntimeVersionIdentity {
                    route: AgentRuntimeRouteKey {
                        agent_id: AgentId::parse(agent_id).unwrap(),
                        transport_kind: TransportKind::Acp,
                        adapter_id: AcpAdapterId::parse(adapter_id).unwrap(),
                    },
                    adapter_version: Some(adapter_version.to_string()),
                    agent_version,
                    runtime_dependencies,
                    source: AgentVersionSource::Managed,
                },
                command: adapter_id.to_string(),
                args: Vec::new(),
                safe_env_references: Vec::new(),
                cwd_template: Some("{workspaceRoot}".to_string()),
                process_strategy: AcpProcessStrategy::PerSession,
                runtime_home_strategy: AgentRuntimeHomeStrategy::VibexPrivate,
                host_capabilities: AgentHostCapabilities::default(),
                resource_policy: AgentRuntimeResourcePolicy {
                    sandbox: ProviderSandboxDefaults::workspace_write_ask_on_risk(),
                    network: ProviderNetworkDefaults::local_default(),
                    permissions: ProviderPermissionDefaults::ask_on_risk(),
                },
            }
        };
        let claude_runtime = service
            .create_agent_runtime_profile(runtime_request("claude", "claude-agent-acp", "0.64.2"))
            .unwrap();
        let codex_runtime = service
            .create_agent_runtime_profile(runtime_request("codex", "codex-acp", "1.1.9"))
            .unwrap();
        for (agent_id, runtime, descriptor_id, endpoint_id) in [
            (
                "claude",
                claude_runtime,
                vibex_core::CLAUDE_PROJECTION_DESCRIPTOR_ID,
                "claude-api",
            ),
            (
                "codex",
                codex_runtime,
                vibex_core::CODEX_PROJECTION_DESCRIPTOR_ID,
                "codex-api",
            ),
        ] {
            service
                .create_agent_model_provider_binding(AgentModelProviderBindingCreateRequest {
                    agent_id: AgentId::parse(agent_id).unwrap(),
                    runtime_profile_id: runtime.id,
                    model_provider_profile_id: provider.id.clone(),
                    projection_descriptor_id: AgentProviderProjectionDescriptorId::parse(
                        descriptor_id,
                    )
                    .unwrap(),
                    projection_overrides: AgentProviderProjectionOverrides {
                        endpoint_id: Some(endpoint_id.to_string()),
                        ..AgentProviderProjectionOverrides::default()
                    },
                    configured_models: Vec::new(),
                })
                .unwrap();
        }

        let mut metadata_only = provider;
        let metadata_revision = metadata_only.revision;
        metadata_only.revision = metadata_revision.saturating_add(1);
        metadata_only
            .configured_models
            .push(ModelProviderCatalogEntry {
                id: "unused-disabled-model".to_string(),
                display_name: Some("Unused".to_string()),
                enabled: false,
                metadata: Vec::new(),
                capabilities: Default::default(),
            });
        let mut updated = service
            .update_model_provider_profile(ModelProviderProfileUpdateRequest {
                profile: metadata_only,
                expected_revision: metadata_revision,
            })
            .unwrap();
        let ready = service
            .list_agent_model_provider_bindings(AgentModelProviderBindingListRequest {
                agent_id: None,
                model_provider_profile_id: Some(updated.id.clone()),
            })
            .unwrap();
        assert!(
            ready
                .iter()
                .all(|binding| binding.status == AgentModelProviderBindingStatus::Ready)
        );

        let endpoint_revision = updated.revision;
        updated.revision = endpoint_revision.saturating_add(1);
        updated.endpoints[0].url = "https://claude-new.example.invalid/v1".to_string();
        let updated = service
            .update_model_provider_profile(ModelProviderProfileUpdateRequest {
                profile: updated,
                expected_revision: endpoint_revision,
            })
            .unwrap();
        let bindings = service
            .list_agent_model_provider_bindings(AgentModelProviderBindingListRequest {
                agent_id: None,
                model_provider_profile_id: Some(updated.id),
            })
            .unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(
            bindings
                .iter()
                .find(|binding| binding.agent_id.as_str() == "claude")
                .unwrap()
                .status,
            AgentModelProviderBindingStatus::StaleRestartRequired
        );
        assert_eq!(
            bindings
                .iter()
                .find(|binding| binding.agent_id.as_str() == "codex")
                .unwrap()
                .status,
            AgentModelProviderBindingStatus::Ready
        );
    }
}

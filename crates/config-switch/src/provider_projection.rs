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
    ProviderBindingMetadata, ProviderCredentialSecretMutationRequest, ProviderSecretBackend,
    ProviderSecretSetupState, ProviderSwitchBehavior, RequestId, VibexError, VibexResult,
    unix_timestamp_ms,
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
    pub session_config: Vec<ProviderBindingMetadata>,
    pub effective_model: Option<String>,
    pub switch_behavior: ProviderSwitchBehavior,
    pub fingerprint: String,
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
            .field("effective_model", &self.effective_model)
            .field("switch_behavior", &self.switch_behavior)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AgentProviderProjectionEngine;

impl AgentProviderProjectionEngine {
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

        let endpoint = selected_endpoint(model_provider, binding)?;
        let credential = selected_credential(model_provider, binding)?;
        if let Some(credential) = credential
            && !descriptor
                .credential_kinds
                .contains(&credential.credential.kind())
            && !descriptor.credential_kinds.is_empty()
        {
            return Err(VibexError::validation(
                "agent_projection_credential_kind_unsupported",
                "selected credential kind is not supported by the exact projection descriptor",
            ));
        }
        let selected_model = selected_model(model_provider, binding)?;
        let effective_model = selected_model.map(|model| model.agent_model_id.clone());

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
            write_private_file_atomic(&path, overlay.content.as_bytes())?;
            overlay_files.push(path);
        }
        let mut non_secret_env = plan.non_secret_env.clone();
        if plan
            .overlay_files
            .iter()
            .any(|overlay| overlay.relative_path == "config.toml")
        {
            non_secret_env.insert(
                "CODEX_HOME".to_string(),
                overlay_root.to_string_lossy().into_owned(),
            );
        }
        Ok(ResolvedAgentProviderProjection {
            binding_id: plan.binding_id.clone(),
            non_secret_env,
            secret_env,
            overlay_root,
            overlay_files,
            session_config: plan.session_config.clone(),
            effective_model: plan.effective_model.clone(),
            switch_behavior: plan.switch_behavior,
            fingerprint: plan.fingerprint.clone(),
        })
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
                "binding descriptor does not match the exact runtime identity",
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
                "binding descriptor does not match the exact runtime identity",
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
    ) -> VibexResult<()> {
        let records = ProviderProjectionCompatibilityRepository::sync_legacy_profile(conn, legacy)?;
        self.refresh_one_binding(conn, records.binding)
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
    ) -> VibexResult<()> {
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
) -> VibexResult<Option<&'a ModelProviderEndpoint>> {
    if let Some(endpoint_id) = binding.projection_overrides.endpoint_id.as_deref() {
        return provider
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == endpoint_id)
            .map(Some)
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_projection_endpoint_not_found",
                    "binding endpoint override was not found in the model provider profile",
                )
            });
    }
    Ok(provider.primary_api_endpoint())
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
                env.insert(key.clone(), endpoint.url.clone());
                targets.push(endpoint_target(key, &endpoint.url));
            }
        }
        AgentProviderControl::ManagedConfigOverlay { strategy } => {
            let overlay = build_overlay(strategy, provider, binding, endpoint, model)?;
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
            secret_env.push(ProjectionSecretEnvReference {
                key: secret_env_key.clone(),
                credential_id: credential.id.clone(),
                secret_reference: secret_reference.clone(),
            });
            targets.push(ProjectionTargetPreview {
                field: "credential".to_string(),
                target_kind: ProjectionTargetKind::Environment,
                target: secret_env_key.clone(),
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

fn build_overlay(
    strategy: &ConfigOverlayStrategy,
    provider: &ModelProviderProfile,
    binding: &AgentModelProviderBinding,
    endpoint: Option<&ModelProviderEndpoint>,
    model: Option<&AgentConfiguredModelBinding>,
) -> VibexResult<ManagedProjectionOverlay> {
    let (relative_path, format, content, contains_secret_reference) = match strategy {
        ConfigOverlayStrategy::CodexStableHome => (
            "config.toml",
            "toml",
            codex_overlay(provider, endpoint),
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
) -> String {
    let provider_id = sanitize_provider_id(
        provider
            .vendor_hint
            .as_deref()
            .unwrap_or_else(|| provider.id.as_str()),
    );
    let mut lines = vec![format!("model_provider = {}", toml_quote(&provider_id))];
    lines.push(String::new());
    lines.push(format!("[model_providers.{}]", toml_quote(&provider_id)));
    lines.push(format!("name = {}", toml_quote(&provider.display_name)));
    if let Some(endpoint) = endpoint {
        lines.push(format!("base_url = {}", toml_quote(&endpoint.url)));
    }
    lines.push("wire_api = \"responses\"".to_string());
    lines.push("requires_openai_auth = true".to_string());
    lines.push("env_key = \"CODEX_API_KEY\"".to_string());
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
        let entry = providers.entry(provider_id).or_insert_with(|| {
            serde_json::json!({
                "name": provider.display_name,
                "npm": npm,
                "options": {},
                "models": {}
            })
        });
        if let Some(models) = entry
            .get_mut("models")
            .and_then(serde_json::Value::as_object_mut)
        {
            models.insert(
                model.agent_model_id.clone(),
                serde_json::json!({
                    "name": provider
                        .configured_models
                        .iter()
                        .find(|entry| entry.id == model.provider_model_id)
                        .and_then(|entry| entry.display_name.clone())
                }),
            );
        }
    }
    if providers.is_empty() {
        providers.insert(
            base_provider_id.clone(),
            serde_json::json!({
                "name": provider.display_name,
                "npm": "@ai-sdk/openai",
                "options": {},
                "models": {}
            }),
        );
    }
    for value in providers.values_mut() {
        if let Some(options) = value
            .get_mut("options")
            .and_then(serde_json::Value::as_object_mut)
        {
            if let Some(endpoint) = endpoint {
                options.insert(
                    "baseURL".to_string(),
                    serde_json::Value::String(endpoint.url.clone()),
                );
            }
            if provider
                .credentials
                .iter()
                .any(|credential| credential.credential.secret_reference().is_some())
            {
                options.insert(
                    "apiKey".to_string(),
                    serde_json::Value::String(format!("{{env:{OPENCODE_SECRET_ENV}}}")),
                );
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
                format!("{provider_id}/{}", model.agent_model_id)
            })
            .unwrap_or_else(|| format!("{base_provider_id}/{default_model}"));
        root.insert("model".to_string(), serde_json::Value::String(qualified));
    }
    serde_json::to_string(&root).map_err(encode_error)
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
        _ => "custom",
    };
    let provider_id = if suffix == "responses" {
        base_provider_id.to_string()
    } else {
        format!("{base_provider_id}-{suffix}")
    };
    (provider_id, npm)
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
        "providerId": provider.id,
        "providerStatus": provider.status,
        "endpoint": selected_endpoint(provider, binding)?.map(|value| &value.url),
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
            "persisted binding descriptor does not match the exact runtime identity",
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
            ProjectionEvidenceState::Unverified => AgentModelProviderBindingStatus::Unverified,
            ProjectionEvidenceState::Unsupported => AgentModelProviderBindingStatus::Unsupported,
            _ => AgentModelProviderBindingStatus::Ready,
        }
    }
}

fn apply_stale_state(
    binding: &mut AgentModelProviderBinding,
    next_fingerprint: &str,
    match_kind: ProjectionDescriptorMatch,
) {
    if match_kind == ProjectionDescriptorMatch::Conservative
        || binding.verification.state == ProjectionEvidenceState::Unverified
    {
        binding.status = AgentModelProviderBindingStatus::Unverified;
        binding.projection_fingerprint = None;
        binding.verification.state = ProjectionEvidenceState::Unverified;
        return;
    }
    if binding.verification.state == ProjectionEvidenceState::Unsupported {
        binding.status = AgentModelProviderBindingStatus::Unsupported;
        binding.projection_fingerprint = None;
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
) -> VibexResult<()> {
    let existing = require_binding(conn, &binding.id)?;
    if existing.projection_fingerprint.as_deref() == fingerprint
        && existing.status == binding.status
        && existing.verification == binding.verification
    {
        return Ok(());
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
    Ok(())
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
    let output = output.trim_matches('-');
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
        ModelProviderProfileStatus, ProjectionEvidenceReference, ProviderNetworkDefaults,
        ProviderPermissionDefaults, ProviderSandboxDefaults, ProviderSecretKind, TransportKind,
        WIRE_PROTOCOL_OPENAI_RESPONSES,
    };

    use super::*;

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
            }],
            proxy_policy: ModelProviderProxyPolicy::InheritSystem,
            credentials: Vec::new(),
            configured_models: vec![ModelProviderCatalogEntry {
                id: "model-a".to_string(),
                display_name: None,
                enabled: true,
                metadata: Vec::new(),
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
    fn exact_conservative_evidence_never_promotes_a_binding_to_ready() {
        let (_, _, mut binding, mut descriptor) =
            fixture(ConfigOverlayStrategy::StructuredJsonOverlay);

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
                    },
                    ModelProviderEndpoint {
                        id: "codex-api".to_string(),
                        kind: ModelProviderEndpointKind::Api,
                        url: "https://codex.example.invalid/v1".to_string(),
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

//! Session-scoped ACP configuration contracts (plan §9/§10.2).
//!
//! This module deliberately contains no process or attachment side effects.
//! It normalizes option identity, chooses an operation candidate from already
//! negotiated evidence, and builds a provider-neutral model catalog.  The
//! runtime owns the actual wire request and fence/CAS sequencing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vibex_core::{
    AgentConfigStatus, AgentReasoningEffort, AgentRuntimeStatus, AgentSnapshotEntry, ProviderKind,
    ProviderProfileStatus, ProviderProfileSummary, ProviderSessionConfigOption,
    ProviderSessionConfigOptionKind, ProviderSessionConfigValue, RuntimeOptionAvailability,
    SessionConfigValue, SessionRuntimeFeature, SessionRuntimeFeatureKind, SessionRuntimeOption,
    SessionRuntimeOptionCatalog, SessionRuntimeSelection,
};

use crate::protocol::{AcpOperation, AcpOperationStability, AcpWireEncoding, CapabilitySource};
use crate::registry::CapabilitySupport;

pub const CANONICAL_MODEL: &str = "model";
pub const CANONICAL_REASONING_EFFORT: &str = "reasoning_effort";
pub const CANONICAL_APPROVAL_MODE: &str = "approval_mode";
pub const CANONICAL_SANDBOX_MODE: &str = "sandbox_mode";
const MAX_CANONICAL_KEY_LEN: usize = 80;
const MAX_MODEL_ID_LEN: usize = 160;
const MAX_EFFORT_VALUE_LEN: usize = 64;
const MAX_CATALOG_LABEL_LEN: usize = 160;
const RUNTIME_OPTION_CATALOG_DOMAIN: &[u8] = b"vibex/runtime-option-catalog/v2";

/// A normalized semantic option key.  Raw provider ids are never used as
/// authority without passing through this boundary.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CanonicalSessionConfigKey(String);

impl fmt::Debug for CanonicalSessionConfigKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CanonicalSessionConfigKey")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for CanonicalSessionConfigKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl CanonicalSessionConfigKey {
    pub fn parse(value: &str) -> Result<Self, String> {
        let normalized = normalize_identifier(value);
        if normalized.is_empty() {
            return Err("session config key must not be empty".to_string());
        }
        if normalized.len() > MAX_CANONICAL_KEY_LEN {
            return Err("session config key is too long".to_string());
        }
        Ok(Self(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_reserved(&self) -> bool {
        matches!(
            self.as_str(),
            CANONICAL_MODEL
                | CANONICAL_REASONING_EFFORT
                | CANONICAL_APPROVAL_MODE
                | CANONICAL_SANDBOX_MODE
        )
    }
}

/// Normalizes identifiers without guessing semantic meaning.  A semantic
/// alias still has to be explicitly registered by the exact adapter
/// compatibility descriptor.
pub fn normalize_identifier(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.trim().chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !output.is_empty() {
                output.push('_');
            }
            output.push(character.to_ascii_lowercase());
            separator = false;
        } else if matches!(character, '_' | '-' | ' ' | '\t' | '.') {
            separator = true;
        }
    }
    output.trim_matches('_').to_string()
}

/// Resolves an Agent option id using explicit exact-identity aliases.  The
/// four canonical spellings are safe on their own; alternate spellings need
/// a Registry alias and collisions fail closed.
pub fn resolve_canonical_option_key(
    raw_id: &str,
    aliases: &BTreeMap<String, Vec<String>>,
) -> Result<CanonicalSessionConfigKey, CanonicalKeyError> {
    let normalized = normalize_identifier(raw_id);
    if normalized.is_empty() {
        return Err(CanonicalKeyError::Invalid(raw_id.to_string()));
    }
    let direct = CanonicalSessionConfigKey::parse(&normalized)
        .map_err(|_| CanonicalKeyError::Invalid(raw_id.to_string()))?;
    let mut matches = BTreeSet::new();
    // The four reserved spellings are authoritative.  An adapter alias that
    // also claims one of those spellings is a conflict, rather than a reason
    // to silently reinterpret the reserved field.
    if direct.is_reserved() {
        matches.insert(direct.clone());
    }
    for (canonical, values) in aliases {
        let canonical = CanonicalSessionConfigKey::parse(canonical)
            .map_err(|_| CanonicalKeyError::Invalid(canonical.clone()))?;
        if normalize_identifier(canonical.as_str()) == normalized
            || values
                .iter()
                .any(|alias| normalize_identifier(alias) == normalized)
        {
            matches.insert(canonical);
        }
    }
    if matches.len() > 1 {
        return Err(CanonicalKeyError::Ambiguous(
            matches.into_iter().map(|key| key.0).collect(),
        ));
    }
    if let Some(key) = matches.into_iter().next() {
        return Ok(key);
    }
    Ok(direct)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalKeyError {
    Invalid(String),
    Ambiguous(Vec<String>),
}

impl CanonicalKeyError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "acp_session_config_key_invalid",
            Self::Ambiguous(_) => "acp_session_config_key_ambiguous",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionConfigFieldKind {
    Model,
    Mode,
    ReasoningEffort,
    Generic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfigFieldRequest {
    pub key: CanonicalSessionConfigKey,
    pub kind: SessionConfigFieldKind,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfigOperationEvidence {
    pub support: CapabilitySupport,
    pub source: CapabilitySource,
    pub encoding: AcpWireEncoding,
    pub stability: AcpOperationStability,
    pub compatibility_identity: String,
    pub activation_generation: i64,
}

impl SessionConfigOperationEvidence {
    pub fn supported_for(&self, identity: &str, generation: i64) -> bool {
        self.support == CapabilitySupport::Supported
            && self.compatibility_identity == identity
            && self.activation_generation == generation
    }

    pub fn unsupported_for(&self, identity: &str, generation: i64) -> bool {
        self.support == CapabilitySupport::Unsupported
            && self.compatibility_identity == identity
            && self.activation_generation == generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfigExtension {
    pub id: String,
    pub operation: AcpOperation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionConfigPlan {
    Live {
        operation: AcpOperation,
        encoding: AcpWireEncoding,
        source: CapabilitySource,
        option_id: Option<String>,
    },
    Extension {
        id: String,
        operation: AcpOperation,
    },
    RestartAndResume,
    Unavailable,
}

impl SessionConfigPlan {
    pub fn operation(&self) -> Option<&AcpOperation> {
        match self {
            Self::Live { operation, .. } | Self::Extension { operation, .. } => Some(operation),
            Self::RestartAndResume | Self::Unavailable => None,
        }
    }

    pub fn encoding_name(&self) -> Option<&'static str> {
        match self {
            Self::Live { encoding, .. } => Some(match encoding {
                AcpWireEncoding::Typed => "typed",
                AcpWireEncoding::VersionedRaw => "versioned_raw",
                AcpWireEncoding::ExtensionCodec => "extension",
            }),
            Self::Extension { .. } => Some("extension"),
            Self::RestartAndResume | Self::Unavailable => None,
        }
    }
}

/// Session discovery data consumed by the operation planner.  It intentionally
/// has no process/attachment ownership and can be reconstructed per generation.
#[derive(Clone)]
pub struct SessionConfigPlanner {
    compatibility_identity: String,
    activation_generation: i64,
    aliases: BTreeMap<String, Vec<String>>,
    operations: BTreeMap<AcpOperation, SessionConfigOperationEvidence>,
    operation_fallbacks: BTreeMap<AcpOperation, Vec<SessionConfigOperationEvidence>>,
    options: Vec<ProviderSessionConfigOption>,
    extensions: BTreeMap<String, SessionConfigExtension>,
    startup_projections: BTreeSet<String>,
}

impl fmt::Debug for SessionConfigPlanner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionConfigPlanner")
            .field("compatibility_identity", &self.compatibility_identity)
            .field("activation_generation", &self.activation_generation)
            .field("operation_count", &self.operations.len())
            .field("option_count", &self.options.len())
            .field("extension_count", &self.extensions.len())
            .finish()
    }
}

impl SessionConfigPlanner {
    pub fn new(
        compatibility_identity: impl Into<String>,
        activation_generation: i64,
        aliases: BTreeMap<String, Vec<String>>,
        operations: BTreeMap<AcpOperation, SessionConfigOperationEvidence>,
        options: Vec<ProviderSessionConfigOption>,
    ) -> Self {
        Self {
            compatibility_identity: compatibility_identity.into(),
            activation_generation,
            aliases,
            operations,
            operation_fallbacks: BTreeMap::new(),
            options,
            extensions: BTreeMap::new(),
            startup_projections: BTreeSet::new(),
        }
    }

    pub fn with_extension(
        mut self,
        key: impl Into<String>,
        extension: SessionConfigExtension,
    ) -> Self {
        self.extensions
            .insert(normalize_identifier(&key.into()), extension);
        self
    }

    pub fn with_startup_projection(mut self, key: impl Into<String>) -> Self {
        self.startup_projections
            .insert(normalize_identifier(&key.into()));
        self
    }

    /// Adds a lower-priority encoding for an operation.  This is useful when a
    /// negotiated typed candidate has an explicitly versioned raw fallback;
    /// the map's primary entry remains the preferred candidate.
    pub fn with_operation_fallback(
        mut self,
        operation: AcpOperation,
        evidence: SessionConfigOperationEvidence,
    ) -> Self {
        self.operation_fallbacks
            .entry(operation)
            .or_default()
            .push(evidence);
        self
    }

    pub fn option_key(
        &self,
        option_id: &str,
    ) -> Result<CanonicalSessionConfigKey, CanonicalKeyError> {
        resolve_canonical_option_key(option_id, &self.aliases)
    }

    pub fn option_for_key(
        &self,
        key: &CanonicalSessionConfigKey,
    ) -> Result<Option<&ProviderSessionConfigOption>, CanonicalKeyError> {
        let mut found = None;
        for option in &self.options {
            if self.option_key(&option.id)? == *key {
                if found.is_some() {
                    return Err(CanonicalKeyError::Ambiguous(vec![key.to_string()]));
                }
                found = Some(option);
            }
        }
        Ok(found)
    }

    pub fn plan(
        &self,
        field: &SessionConfigFieldRequest,
    ) -> Result<SessionConfigPlan, CanonicalKeyError> {
        let config_option = self.option_for_key(&field.key)?;
        let config_evidence = self
            .supported_operation_candidates(&AcpOperation::SessionSetConfigOption)
            .into_iter()
            .next();

        let plan = match field.kind {
            SessionConfigFieldKind::Model => {
                self.plan_model(&field.key, config_option, config_evidence)
            }
            SessionConfigFieldKind::Mode => self.plan_mode(&field.key),
            SessionConfigFieldKind::ReasoningEffort | SessionConfigFieldKind::Generic => {
                config_evidence
                    .filter(|_| config_option.is_some())
                    .map(|evidence| SessionConfigPlan::Live {
                        operation: AcpOperation::SessionSetConfigOption,
                        encoding: evidence.encoding,
                        source: evidence.source,
                        option_id: config_option.map(|option| option.id.clone()),
                    })
                    .or_else(|| self.extension_for(&field.key))
                    .unwrap_or_else(|| self.restart_or_unavailable(&field.key))
            }
        };
        Ok(plan)
    }

    /// Re-plan after an observed method-not-found/unsupported response.  The
    /// negative is scoped to this planner's exact identity and generation;
    /// static registry evidence is therefore unable to re-enable the same
    /// operation during the current attachment.
    pub fn plan_after_capability_negative(
        &self,
        field: &SessionConfigFieldRequest,
    ) -> Result<SessionConfigPlan, CanonicalKeyError> {
        let operation = match field.kind {
            SessionConfigFieldKind::Model => Some(AcpOperation::SessionSetModel),
            SessionConfigFieldKind::Mode => Some(AcpOperation::SessionSetMode),
            SessionConfigFieldKind::ReasoningEffort | SessionConfigFieldKind::Generic => {
                Some(AcpOperation::SessionSetConfigOption)
            }
        };
        let next = operation
            .as_ref()
            .map(|operation| self.with_capability_negative(operation))
            .unwrap_or_else(|| self.clone());
        next.plan(field)
    }

    pub fn with_capability_negative(&self, operation: &AcpOperation) -> Self {
        let mut next = self.clone();
        if let Some(evidence) = next.operations.get_mut(operation) {
            evidence.support = CapabilitySupport::Unsupported;
            evidence.source = CapabilitySource::ObservedRuntime;
        }
        next.extensions
            .retain(|_, extension| &extension.operation != operation);
        next
    }

    fn plan_model(
        &self,
        key: &CanonicalSessionConfigKey,
        config_option: Option<&ProviderSessionConfigOption>,
        config_evidence: Option<&SessionConfigOperationEvidence>,
    ) -> SessionConfigPlan {
        for evidence in self.supported_operation_candidates(&AcpOperation::SessionSetModel) {
            if matches!(
                evidence.encoding,
                AcpWireEncoding::Typed | AcpWireEncoding::VersionedRaw
            ) {
                return SessionConfigPlan::Live {
                    operation: AcpOperation::SessionSetModel,
                    encoding: evidence.encoding,
                    source: evidence.source,
                    option_id: None,
                };
            }
        }
        config_evidence
            .filter(|_| config_option.is_some())
            .map(|evidence| SessionConfigPlan::Live {
                operation: AcpOperation::SessionSetConfigOption,
                encoding: evidence.encoding,
                source: evidence.source,
                option_id: config_option.map(|option| option.id.clone()),
            })
            .or_else(|| self.extension_for(key))
            .unwrap_or_else(|| self.restart_or_unavailable(key))
    }

    fn plan_mode(&self, key: &CanonicalSessionConfigKey) -> SessionConfigPlan {
        for evidence in self.supported_operation_candidates(&AcpOperation::SessionSetMode) {
            if matches!(
                evidence.encoding,
                AcpWireEncoding::Typed | AcpWireEncoding::VersionedRaw
            ) {
                return SessionConfigPlan::Live {
                    operation: AcpOperation::SessionSetMode,
                    encoding: evidence.encoding,
                    source: evidence.source,
                    option_id: None,
                };
            }
        }
        self.extension_for(key)
            .unwrap_or_else(|| self.restart_or_unavailable(key))
    }

    fn extension_for(&self, key: &CanonicalSessionConfigKey) -> Option<SessionConfigPlan> {
        self.extensions
            .get(key.as_str())
            .map(|extension| SessionConfigPlan::Extension {
                id: extension.id.clone(),
                operation: extension.operation.clone(),
            })
    }

    fn restart_or_unavailable(&self, key: &CanonicalSessionConfigKey) -> SessionConfigPlan {
        if self.startup_projections.contains(key.as_str()) {
            SessionConfigPlan::RestartAndResume
        } else {
            SessionConfigPlan::Unavailable
        }
    }

    fn supported_operation_candidates(
        &self,
        operation: &AcpOperation,
    ) -> Vec<&SessionConfigOperationEvidence> {
        let mut candidates = Vec::new();
        if let Some(evidence) = self.operations.get(operation)
            && evidence.supported_for(&self.compatibility_identity, self.activation_generation)
        {
            candidates.push(evidence);
        }
        if let Some(fallbacks) = self.operation_fallbacks.get(operation) {
            let mut fallbacks = fallbacks
                .iter()
                .filter(|evidence| {
                    evidence.supported_for(&self.compatibility_identity, self.activation_generation)
                })
                .collect::<Vec<_>>();
            fallbacks.sort_by_key(|evidence| operation_candidate_rank(evidence));
            candidates.extend(fallbacks);
        }
        candidates.sort_by_key(|evidence| operation_candidate_rank(evidence));
        candidates
    }

    pub fn ordered_fields(
        fields: impl IntoIterator<Item = SessionConfigFieldRequest>,
    ) -> Vec<SessionConfigFieldRequest> {
        let mut fields = fields.into_iter().collect::<Vec<_>>();
        fields.sort_by(|left, right| {
            field_order(left.kind)
                .cmp(&field_order(right.kind))
                .then_with(|| left.key.cmp(&right.key))
        });
        fields
    }
}

fn operation_candidate_rank(evidence: &SessionConfigOperationEvidence) -> (u8, u8) {
    let source = match evidence.source {
        CapabilitySource::NegotiatedRuntime => 0,
        CapabilitySource::ObservedRuntime => 1,
        CapabilitySource::VersionedRegistry => 2,
        CapabilitySource::DeclaredProfile => 3,
        CapabilitySource::FixedSchema => 4,
        CapabilitySource::ConservativeDefault => 5,
    };
    let encoding = match evidence.encoding {
        AcpWireEncoding::Typed => 0,
        AcpWireEncoding::VersionedRaw => 1,
        AcpWireEncoding::ExtensionCodec => 2,
    };
    (source, encoding)
}

fn field_order(kind: SessionConfigFieldKind) -> u8 {
    match kind {
        SessionConfigFieldKind::Model => 0,
        SessionConfigFieldKind::ReasoningEffort => 1,
        SessionConfigFieldKind::Mode => 2,
        SessionConfigFieldKind::Generic => 3,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionModelCatalogSource {
    Session,
    Probe,
    Profile,
    Extension,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelCatalogEntry {
    pub model_id: String,
    pub reasoning_efforts: Vec<AgentReasoningEffort>,
    pub default_reasoning_effort: Option<String>,
    pub source: SessionModelCatalogSource,
}

/// Capability evidence scoped to one Provider Profile. It contains only
/// product-safe values: callers keep adapter identity, raw payloads and native
/// ids outside the Runtime Option Catalog boundary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeOptionCatalogProfileEvidence {
    pub models: Vec<SessionModelCatalogEntry>,
    pub modes: Vec<ProviderSessionConfigValue>,
    pub options: Vec<ProviderSessionConfigOption>,
    pub temporarily_unavailable: bool,
}

/// Builds the redacted, deterministic Runtime Option Catalog consumed by
/// ordinary session selectors.
pub fn build_runtime_option_catalog(
    agents: &[AgentSnapshotEntry],
    profiles: &[ProviderProfileSummary],
    evidence_by_profile: &BTreeMap<
        vibex_core::ProviderProfileId,
        RuntimeOptionCatalogProfileEvidence,
    >,
) -> SessionRuntimeOptionCatalog {
    let agents_by_id = agents
        .iter()
        .filter(|agent| agent.added && agent.enabled)
        .map(|agent| (agent.id.clone(), agent))
        .collect::<BTreeMap<_, _>>();
    let mut enabled_profiles = profiles
        .iter()
        .filter(|profile| {
            profile.kind == ProviderKind::Acp && profile.status == ProviderProfileStatus::Enabled
        })
        .filter_map(|profile| {
            agents_by_id
                .get(&profile.agent_id)
                .map(|agent| (*agent, profile))
        })
        .collect::<Vec<_>>();
    enabled_profiles.sort_by(|(left_agent, left_profile), (right_agent, right_profile)| {
        left_agent
            .order_index
            .cmp(&right_agent.order_index)
            .then_with(|| left_agent.id.cmp(&right_agent.id))
            .then_with(|| left_profile.display_name.cmp(&right_profile.display_name))
            .then_with(|| left_profile.id.cmp(&right_profile.id))
    });

    let mut options = Vec::new();
    for (agent, profile) in enabled_profiles {
        let evidence = evidence_by_profile.get(&profile.id);
        let evidence_models = evidence
            .map(|entry| {
                entry
                    .models
                    .iter()
                    .map(|model| (model.model_id.trim().to_string(), model))
                    .filter(|(model_id, _)| {
                        !model_id.is_empty() && model_id.len() <= MAX_MODEL_ID_LEN
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let mut configured_models = profile
            .configured_models
            .iter()
            .filter(|model| model.enabled)
            .filter_map(|model| {
                let id = validate_model_value(&model.id).ok()?;
                let label = bounded_catalog_label(model.display_name.as_deref().unwrap_or(&id));
                Some((id, label))
            })
            .collect::<BTreeMap<_, _>>();
        let has_explicit_model_configuration = !profile.configured_models.is_empty()
            || profile
                .default_model
                .as_deref()
                .is_some_and(|model| !model.trim().is_empty());
        if configured_models.is_empty()
            && let Some(model_id) = profile
                .default_model
                .as_deref()
                .and_then(|model| validate_model_value(model).ok())
        {
            configured_models.insert(model_id.clone(), model_id);
        }
        if configured_models.is_empty() && !has_explicit_model_configuration {
            configured_models.extend(
                evidence_models
                    .keys()
                    .map(|model_id| (model_id.clone(), model_id.clone())),
            );
        }

        let availability = catalog_availability(agent, evidence);
        let modes = catalog_modes(evidence);
        let features = catalog_features(evidence);
        let feature_config_values = features
            .iter()
            .filter_map(|feature| {
                feature
                    .current_value
                    .as_ref()
                    .or(feature.default_value.as_ref())
                    .map(|value| (feature.id.clone(), value.value.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        for (model_id, model_label) in configured_models {
            let model_evidence = evidence_models.get(&model_id).copied();
            options.push(SessionRuntimeOption {
                selection: SessionRuntimeSelection {
                    agent_id: agent.id.clone(),
                    provider_profile_id: profile.id.clone(),
                    model_id,
                    reasoning_effort: None,
                    mode_id: None,
                    config_values: feature_config_values.clone(),
                },
                agent_label: bounded_catalog_label(&agent.label),
                provider_profile_label: bounded_catalog_label(&profile.display_name),
                model_label,
                reasoning_efforts: model_evidence
                    .map(catalog_reasoning_efforts)
                    .unwrap_or_default(),
                modes: modes.clone(),
                features: features.clone(),
                availability,
            });
        }
    }

    SessionRuntimeOptionCatalog {
        revision: runtime_option_catalog_revision(&options),
        options,
    }
}

fn catalog_availability(
    agent: &AgentSnapshotEntry,
    evidence: Option<&RuntimeOptionCatalogProfileEvidence>,
) -> RuntimeOptionAvailability {
    if matches!(
        agent.config_status,
        AgentConfigStatus::NeedsConfiguration | AgentConfigStatus::Unknown
    ) {
        RuntimeOptionAvailability::RequiresConfiguration
    } else if evidence.is_some_and(|value| value.temporarily_unavailable)
        || matches!(
            agent.runtime_status,
            AgentRuntimeStatus::Unavailable | AgentRuntimeStatus::ProbeFailed
        )
    {
        RuntimeOptionAvailability::TemporarilyUnavailable
    } else {
        RuntimeOptionAvailability::Available
    }
}

fn catalog_modes(
    evidence: Option<&RuntimeOptionCatalogProfileEvidence>,
) -> Vec<SessionConfigValue> {
    let mut modes = evidence
        .into_iter()
        .flat_map(|entry| entry.modes.iter())
        .filter_map(|mode| {
            let value = validate_effort_value(&mode.value).ok()?;
            Some(SessionConfigValue {
                value,
                label: mode.label.as_deref().map(bounded_catalog_label),
            })
        })
        .collect::<Vec<_>>();
    modes.sort_by(|left, right| left.value.cmp(&right.value));
    modes.dedup_by(|left, right| left.value == right.value);
    modes
}

fn catalog_reasoning_efforts(model: &SessionModelCatalogEntry) -> Vec<SessionConfigValue> {
    let mut efforts = model
        .reasoning_efforts
        .iter()
        .filter_map(|effort| {
            let value = validate_effort_value(&effort.value).ok()?;
            Some(SessionConfigValue {
                value,
                label: effort.description.as_deref().map(bounded_catalog_label),
            })
        })
        .collect::<Vec<_>>();
    efforts.sort_by(|left, right| left.value.cmp(&right.value));
    efforts.dedup_by(|left, right| left.value == right.value);
    efforts
}

fn catalog_features(
    evidence: Option<&RuntimeOptionCatalogProfileEvidence>,
) -> Vec<SessionRuntimeFeature> {
    let mut features = evidence
        .into_iter()
        .flat_map(|entry| entry.options.iter())
        .filter_map(|option| {
            let id = normalize_identifier(&option.id);
            if id.is_empty()
                || id.len() > MAX_CANONICAL_KEY_LEN
                || is_structural_catalog_option(option, &id)
            {
                return None;
            }
            let kind = match &option.kind {
                ProviderSessionConfigOptionKind::Boolean => SessionRuntimeFeatureKind::Toggle,
                ProviderSessionConfigOptionKind::Select => SessionRuntimeFeatureKind::Select,
                ProviderSessionConfigOptionKind::String => SessionRuntimeFeatureKind::String,
            };
            let mut values = option
                .values
                .iter()
                .filter_map(catalog_feature_value)
                .collect::<Vec<_>>();
            values.sort_by(|left, right| left.value.cmp(&right.value));
            values.dedup_by(|left, right| left.value == right.value);
            let current_value = option
                .current_value
                .as_ref()
                .and_then(catalog_feature_value)
                .map(|value| enrich_feature_value_label(value, &values));
            let default_value = option
                .default_value
                .as_ref()
                .and_then(catalog_feature_value)
                .map(|value| enrich_feature_value_label(value, &values));
            Some(SessionRuntimeFeature {
                id,
                label: bounded_catalog_label(&option.label),
                description: option.description.as_deref().map(bounded_catalog_label),
                kind,
                current_value,
                default_value,
                values,
            })
        })
        .collect::<Vec<_>>();
    features.sort_by(|left, right| left.id.cmp(&right.id));
    features.dedup_by(|left, right| left.id == right.id);
    features
}

fn is_structural_catalog_option(option: &ProviderSessionConfigOption, id: &str) -> bool {
    const STRUCTURAL_KEYS: [&str; 6] = [
        CANONICAL_MODEL,
        "mode",
        CANONICAL_REASONING_EFFORT,
        "effort",
        "thinking_level",
        "thought_level",
    ];
    STRUCTURAL_KEYS.contains(&id)
        || option
            .category
            .as_deref()
            .map(normalize_identifier)
            .is_some_and(|category| STRUCTURAL_KEYS.contains(&category.as_str()))
}

fn catalog_feature_value(value: &ProviderSessionConfigValue) -> Option<SessionConfigValue> {
    let normalized = value.value.trim();
    if normalized.is_empty() || normalized.len() > 256 {
        return None;
    }
    Some(SessionConfigValue {
        value: normalized.to_string(),
        label: value.label.as_deref().map(bounded_catalog_label),
    })
}

fn enrich_feature_value_label(
    mut value: SessionConfigValue,
    candidates: &[SessionConfigValue],
) -> SessionConfigValue {
    if value.label.is_none() {
        value.label = candidates
            .iter()
            .find(|candidate| candidate.value == value.value)
            .and_then(|candidate| candidate.label.clone());
    }
    value
}

fn bounded_catalog_label(value: &str) -> String {
    let value = value.trim();
    if value.len() <= MAX_CATALOG_LABEL_LEN {
        return value.to_string();
    }
    let mut end = MAX_CATALOG_LABEL_LEN;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn runtime_option_catalog_revision(options: &[SessionRuntimeOption]) -> i64 {
    let mut hasher = Sha256::new();
    write_catalog_component(&mut hasher, RUNTIME_OPTION_CATALOG_DOMAIN);
    for option in options {
        write_catalog_component(&mut hasher, option.selection.agent_id.as_str().as_bytes());
        write_catalog_component(
            &mut hasher,
            option.selection.provider_profile_id.as_str().as_bytes(),
        );
        write_catalog_component(&mut hasher, option.selection.model_id.as_bytes());
        write_catalog_component(&mut hasher, option.agent_label.as_bytes());
        write_catalog_component(&mut hasher, option.provider_profile_label.as_bytes());
        write_catalog_component(&mut hasher, option.model_label.as_bytes());
        write_catalog_component(
            &mut hasher,
            match option.availability {
                RuntimeOptionAvailability::Available => b"available",
                RuntimeOptionAvailability::TemporarilyUnavailable => b"temporarily_unavailable",
                RuntimeOptionAvailability::RequiresConfiguration => b"requires_configuration",
            },
        );
        for effort in &option.reasoning_efforts {
            write_catalog_component(&mut hasher, b"effort");
            write_catalog_component(&mut hasher, effort.value.as_bytes());
            write_catalog_component(
                &mut hasher,
                effort.label.as_deref().unwrap_or_default().as_bytes(),
            );
        }
        for mode in &option.modes {
            write_catalog_component(&mut hasher, b"mode");
            write_catalog_component(&mut hasher, mode.value.as_bytes());
            write_catalog_component(
                &mut hasher,
                mode.label.as_deref().unwrap_or_default().as_bytes(),
            );
        }
        for feature in &option.features {
            write_catalog_component(&mut hasher, b"feature");
            write_catalog_component(&mut hasher, feature.id.as_bytes());
            write_catalog_component(&mut hasher, feature.label.as_bytes());
            write_catalog_component(
                &mut hasher,
                feature
                    .description
                    .as_deref()
                    .unwrap_or_default()
                    .as_bytes(),
            );
            write_catalog_component(
                &mut hasher,
                match feature.kind {
                    SessionRuntimeFeatureKind::Toggle => b"toggle",
                    SessionRuntimeFeatureKind::Select => b"select",
                    SessionRuntimeFeatureKind::String => b"string",
                },
            );
            for (name, value) in [
                (b"current".as_slice(), feature.current_value.as_ref()),
                (b"default".as_slice(), feature.default_value.as_ref()),
            ] {
                write_catalog_component(&mut hasher, name);
                if let Some(value) = value {
                    write_catalog_component(&mut hasher, value.value.as_bytes());
                    write_catalog_component(
                        &mut hasher,
                        value.label.as_deref().unwrap_or_default().as_bytes(),
                    );
                }
            }
            for value in &feature.values {
                write_catalog_component(&mut hasher, b"value");
                write_catalog_component(&mut hasher, value.value.as_bytes());
                write_catalog_component(
                    &mut hasher,
                    value.label.as_deref().unwrap_or_default().as_bytes(),
                );
            }
        }
    }
    let digest = hasher.finalize();
    let mut revision = [0_u8; 8];
    revision.copy_from_slice(&digest[..8]);
    let revision = i64::from_be_bytes(revision) & i64::MAX;
    if revision == 0 { 1 } else { revision }
}

fn write_catalog_component(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

/// Merges model evidence by source priority.  Effort values are attached only
/// when the caller supplies evidence explicitly associated with that model.
pub fn merge_model_catalog(
    session: impl IntoIterator<Item = SessionModelCatalogEntry>,
    probe: impl IntoIterator<Item = SessionModelCatalogEntry>,
    profile_models: impl IntoIterator<Item = String>,
    extensions: impl IntoIterator<Item = SessionModelCatalogEntry>,
) -> Vec<SessionModelCatalogEntry> {
    let mut merged = BTreeMap::<String, SessionModelCatalogEntry>::new();
    for (_source, entries) in [
        (
            SessionModelCatalogSource::Session,
            session.into_iter().collect::<Vec<_>>(),
        ),
        (
            SessionModelCatalogSource::Probe,
            probe.into_iter().collect::<Vec<_>>(),
        ),
        (
            SessionModelCatalogSource::Profile,
            profile_models
                .into_iter()
                .map(|model_id| SessionModelCatalogEntry {
                    model_id,
                    reasoning_efforts: Vec::new(),
                    default_reasoning_effort: None,
                    source: SessionModelCatalogSource::Profile,
                })
                .collect(),
        ),
        (
            SessionModelCatalogSource::Extension,
            extensions.into_iter().collect(),
        ),
    ] {
        for mut entry in entries {
            let model_id = entry.model_id.trim();
            if model_id.is_empty() || model_id.len() > MAX_MODEL_ID_LEN {
                continue;
            }
            entry.model_id = model_id.to_string();
            entry
                .reasoning_efforts
                .sort_by(|left, right| left.value.cmp(&right.value));
            entry
                .reasoning_efforts
                .dedup_by(|left, right| left.value == right.value);
            match merged.get(&entry.model_id) {
                None => {
                    merged.insert(entry.model_id.clone(), entry);
                }
                Some(existing)
                    if existing.reasoning_efforts.is_empty()
                        && !entry.reasoning_efforts.is_empty() =>
                {
                    let existing = merged
                        .get_mut(&entry.model_id)
                        .expect("catalog entry existence checked above");
                    existing.reasoning_efforts = entry.reasoning_efforts;
                    existing.default_reasoning_effort = entry.default_reasoning_effort;
                }
                _ => {}
            }
        }
    }
    merged.into_values().collect()
}

pub fn validate_model_value(value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Err("acp_session_config_model_empty");
    }
    if value.len() > MAX_MODEL_ID_LEN {
        return Err("acp_session_config_model_too_long");
    }
    Ok(value.to_string())
}

pub fn validate_effort_value(value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Err("acp_session_config_effort_empty");
    }
    if value.len() > MAX_EFFORT_VALUE_LEN
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("acp_session_config_effort_invalid");
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        ProviderConfiguredModel, ProviderKind, ProviderProfileStatus, ProviderSecretSetupState,
        ProviderSessionConfigOptionKind, ProviderSessionConfigValue, builtin_agent_definitions,
    };

    fn evidence(
        operation: AcpOperation,
        encoding: AcpWireEncoding,
        generation: i64,
    ) -> (AcpOperation, SessionConfigOperationEvidence) {
        (
            operation,
            SessionConfigOperationEvidence {
                support: CapabilitySupport::Supported,
                source: CapabilitySource::NegotiatedRuntime,
                encoding,
                stability: AcpOperationStability::CapabilityGated,
                compatibility_identity: "adapter=test@1".to_string(),
                activation_generation: generation,
            },
        )
    }

    fn option(id: &str, current: &str) -> ProviderSessionConfigOption {
        ProviderSessionConfigOption {
            id: id.to_string(),
            label: id.to_string(),
            category: None,
            description: None,
            kind: ProviderSessionConfigOptionKind::Select,
            current_value: Some(ProviderSessionConfigValue {
                value: current.to_string(),
                label: None,
            }),
            default_value: None,
            values: Vec::new(),
        }
    }

    fn catalog_option(
        id: &str,
        category: Option<&str>,
        kind: ProviderSessionConfigOptionKind,
        current: &str,
        values: &[(&str, &str)],
    ) -> ProviderSessionConfigOption {
        ProviderSessionConfigOption {
            id: id.to_string(),
            label: id.replace(['-', '_'], " "),
            category: category.map(ToString::to_string),
            description: Some(format!("Configure {id}")),
            kind,
            current_value: Some(ProviderSessionConfigValue {
                value: current.to_string(),
                label: None,
            }),
            default_value: None,
            values: values
                .iter()
                .map(|(value, label)| ProviderSessionConfigValue {
                    value: (*value).to_string(),
                    label: Some((*label).to_string()),
                })
                .collect(),
        }
    }

    #[test]
    fn aliases_are_exact_and_ambiguous_aliases_fail_closed() {
        let aliases = BTreeMap::from([(
            CANONICAL_REASONING_EFFORT.to_string(),
            vec!["effort".to_string(), "thinking-level".to_string()],
        )]);
        assert_eq!(
            resolve_canonical_option_key("thinking_level", &aliases)
                .unwrap()
                .as_str(),
            CANONICAL_REASONING_EFFORT
        );
        let ambiguous = BTreeMap::from([
            (CANONICAL_MODEL.to_string(), vec!["choice".to_string()]),
            (
                CANONICAL_REASONING_EFFORT.to_string(),
                vec!["choice".to_string()],
            ),
        ]);
        assert!(matches!(
            resolve_canonical_option_key("choice", &ambiguous),
            Err(CanonicalKeyError::Ambiguous(_))
        ));
        let reserved_collision = BTreeMap::from([(
            CANONICAL_REASONING_EFFORT.to_string(),
            vec![CANONICAL_MODEL.to_string()],
        )]);
        assert!(matches!(
            resolve_canonical_option_key(CANONICAL_MODEL, &reserved_collision),
            Err(CanonicalKeyError::Ambiguous(_))
        ));
    }

    #[test]
    fn planner_uses_typed_then_raw_then_model_option() {
        let aliases = BTreeMap::from([(CANONICAL_MODEL.to_string(), vec!["model".to_string()])]);
        let operations = BTreeMap::from([evidence(
            AcpOperation::SessionSetModel,
            AcpWireEncoding::VersionedRaw,
            3,
        )]);
        let planner = SessionConfigPlanner::new(
            "adapter=test@1",
            3,
            aliases,
            operations,
            vec![option("model", "m1")],
        );
        let key = CanonicalSessionConfigKey::parse(CANONICAL_MODEL).unwrap();
        let plan = planner
            .plan(&SessionConfigFieldRequest {
                key,
                kind: SessionConfigFieldKind::Model,
                value: "m2".to_string(),
            })
            .unwrap();
        assert!(matches!(
            plan,
            SessionConfigPlan::Live {
                operation: AcpOperation::SessionSetModel,
                encoding: AcpWireEncoding::VersionedRaw,
                ..
            }
        ));
    }

    #[test]
    fn capability_negative_downgrades_only_to_explicit_raw_candidate() {
        let typed = evidence(AcpOperation::SessionSetModel, AcpWireEncoding::Typed, 3);
        let raw = evidence(
            AcpOperation::SessionSetModel,
            AcpWireEncoding::VersionedRaw,
            3,
        );
        let planner = SessionConfigPlanner::new(
            "adapter=test@1",
            3,
            BTreeMap::new(),
            BTreeMap::from([typed]),
            Vec::new(),
        )
        .with_operation_fallback(raw.0, raw.1);
        let request = SessionConfigFieldRequest {
            key: CanonicalSessionConfigKey::parse(CANONICAL_MODEL).unwrap(),
            kind: SessionConfigFieldKind::Model,
            value: "m2".to_string(),
        };
        assert!(matches!(
            planner.plan(&request).unwrap(),
            SessionConfigPlan::Live {
                encoding: AcpWireEncoding::Typed,
                ..
            }
        ));
        assert!(matches!(
            planner.plan_after_capability_negative(&request).unwrap(),
            SessionConfigPlan::Live {
                encoding: AcpWireEncoding::VersionedRaw,
                ..
            }
        ));
    }

    #[test]
    fn config_option_capability_negative_uses_versioned_raw_fallback() {
        let typed = evidence(
            AcpOperation::SessionSetConfigOption,
            AcpWireEncoding::Typed,
            3,
        );
        let raw = evidence(
            AcpOperation::SessionSetConfigOption,
            AcpWireEncoding::VersionedRaw,
            3,
        );
        let planner = SessionConfigPlanner::new(
            "adapter=test@1",
            3,
            BTreeMap::new(),
            BTreeMap::from([typed]),
            vec![option("approval_mode", "ask")],
        )
        .with_operation_fallback(raw.0, raw.1);
        let request = SessionConfigFieldRequest {
            key: CanonicalSessionConfigKey::parse(CANONICAL_APPROVAL_MODE).unwrap(),
            kind: SessionConfigFieldKind::Generic,
            value: "always".to_string(),
        };
        assert!(matches!(
            planner.plan_after_capability_negative(&request).unwrap(),
            SessionConfigPlan::Live {
                encoding: AcpWireEncoding::VersionedRaw,
                ..
            }
        ));
    }

    #[test]
    fn extension_capability_negative_does_not_retry_same_extension() {
        let planner = SessionConfigPlanner::new(
            "adapter=test@1",
            3,
            BTreeMap::new(),
            BTreeMap::new(),
            Vec::new(),
        )
        .with_extension(
            CANONICAL_MODEL,
            SessionConfigExtension {
                id: "vendor-model".to_string(),
                operation: AcpOperation::SessionSetModel,
            },
        )
        .with_startup_projection(CANONICAL_MODEL);
        let request = SessionConfigFieldRequest {
            key: CanonicalSessionConfigKey::parse(CANONICAL_MODEL).unwrap(),
            kind: SessionConfigFieldKind::Model,
            value: "model-b".to_string(),
        };
        assert!(matches!(
            planner.plan_after_capability_negative(&request).unwrap(),
            SessionConfigPlan::RestartAndResume
        ));
    }

    #[test]
    fn planner_does_not_use_stale_generation_evidence() {
        let operations = BTreeMap::from([evidence(
            AcpOperation::SessionSetMode,
            AcpWireEncoding::Typed,
            1,
        )]);
        let planner =
            SessionConfigPlanner::new("adapter=test@1", 2, BTreeMap::new(), operations, Vec::new());
        let key = CanonicalSessionConfigKey::parse("mode").unwrap();
        assert_eq!(
            planner
                .plan(&SessionConfigFieldRequest {
                    key,
                    kind: SessionConfigFieldKind::Mode,
                    value: "build".to_string(),
                })
                .unwrap(),
            SessionConfigPlan::Unavailable
        );
    }

    #[test]
    fn field_order_puts_model_before_effort_and_generic_keys_are_sorted() {
        let model = |key: &str, kind| SessionConfigFieldRequest {
            key: CanonicalSessionConfigKey::parse(key).unwrap(),
            kind,
            value: "v".to_string(),
        };
        let ordered = SessionConfigPlanner::ordered_fields([
            model(CANONICAL_SANDBOX_MODE, SessionConfigFieldKind::Generic),
            model(
                CANONICAL_REASONING_EFFORT,
                SessionConfigFieldKind::ReasoningEffort,
            ),
            model(CANONICAL_MODEL, SessionConfigFieldKind::Model),
            model("approval_mode", SessionConfigFieldKind::Generic),
        ]);
        assert_eq!(ordered[0].kind, SessionConfigFieldKind::Model);
        assert_eq!(ordered[1].kind, SessionConfigFieldKind::ReasoningEffort);
        assert_eq!(ordered[2].key.as_str(), CANONICAL_APPROVAL_MODE);
        assert_eq!(ordered[3].key.as_str(), CANONICAL_SANDBOX_MODE);
    }

    #[test]
    fn catalog_never_fabricates_effort_for_profile_models() {
        let catalog = merge_model_catalog(
            Vec::new(),
            Vec::new(),
            vec!["model-a".to_string(), "model-a".to_string()],
            Vec::new(),
        );
        assert_eq!(catalog.len(), 1);
        assert!(catalog[0].reasoning_efforts.is_empty());
        assert_eq!(catalog[0].default_reasoning_effort, None);
    }

    #[test]
    fn catalog_keeps_higher_priority_source_when_lower_source_adds_effort() {
        let catalog = merge_model_catalog(
            vec![SessionModelCatalogEntry {
                model_id: "model-a".to_string(),
                reasoning_efforts: Vec::new(),
                default_reasoning_effort: None,
                source: SessionModelCatalogSource::Session,
            }],
            Vec::new(),
            Vec::new(),
            vec![SessionModelCatalogEntry {
                model_id: "model-a".to_string(),
                reasoning_efforts: vec![AgentReasoningEffort {
                    value: "high".to_string(),
                    description: None,
                }],
                default_reasoning_effort: Some("high".to_string()),
                source: SessionModelCatalogSource::Extension,
            }],
        );
        assert_eq!(catalog[0].source, SessionModelCatalogSource::Session);
        assert_eq!(catalog[0].reasoning_efforts[0].value, "high");
    }

    #[test]
    fn runtime_option_catalog_is_enabled_redacted_and_deterministic() {
        let definition = builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.id.as_str() == "codex")
            .unwrap();
        let mut agent = AgentSnapshotEntry::from_definition(&definition, None, None);
        agent.added = true;
        agent.enabled = true;
        agent.configured = true;
        agent.config_status = AgentConfigStatus::Configured;
        agent.runtime_status = AgentRuntimeStatus::Ready;
        let profile_id = vibex_core::ProviderProfileId::new();
        let profile = ProviderProfileSummary {
            id: profile_id.clone(),
            agent_id: agent.id.clone(),
            kind: ProviderKind::Acp,
            display_name: "Work profile".to_string(),
            status: ProviderProfileStatus::Enabled,
            account_alias: Some("work".to_string()),
            default_model: Some("gpt-5".to_string()),
            configured_models: vec![
                ProviderConfiguredModel {
                    id: "gpt-5".to_string(),
                    display_name: Some("GPT-5".to_string()),
                    enabled: true,
                    wire_api: None,
                },
                ProviderConfiguredModel {
                    id: "disabled".to_string(),
                    display_name: None,
                    enabled: false,
                    wire_api: None,
                },
            ],
            secret_setup_state: ProviderSecretSetupState::Available,
            updated_at_ms: 123,
        };
        let evidence = BTreeMap::from([(
            profile_id,
            RuntimeOptionCatalogProfileEvidence {
                models: vec![SessionModelCatalogEntry {
                    model_id: "gpt-5".to_string(),
                    reasoning_efforts: vec![AgentReasoningEffort {
                        value: "high".to_string(),
                        description: Some("High".to_string()),
                    }],
                    default_reasoning_effort: Some("high".to_string()),
                    source: SessionModelCatalogSource::Session,
                }],
                modes: vec![ProviderSessionConfigValue {
                    value: "build".to_string(),
                    label: Some("Build".to_string()),
                }],
                options: vec![
                    catalog_option(
                        "model",
                        Some("model"),
                        ProviderSessionConfigOptionKind::Select,
                        "gpt-5",
                        &[("gpt-5", "GPT-5")],
                    ),
                    catalog_option(
                        "thinking-level",
                        Some("thought_level"),
                        ProviderSessionConfigOptionKind::Select,
                        "high",
                        &[("high", "High")],
                    ),
                    catalog_option(
                        "auto-apply",
                        None,
                        ProviderSessionConfigOptionKind::Boolean,
                        "true",
                        &[],
                    ),
                    catalog_option(
                        CANONICAL_APPROVAL_MODE,
                        None,
                        ProviderSessionConfigOptionKind::Select,
                        "ask",
                        &[("ask", "Ask"), ("auto", "Automatic")],
                    ),
                ],
                temporarily_unavailable: false,
            },
        )]);

        let first = build_runtime_option_catalog(
            std::slice::from_ref(&agent),
            std::slice::from_ref(&profile),
            &evidence,
        );
        let second = build_runtime_option_catalog(
            std::slice::from_ref(&agent),
            std::slice::from_ref(&profile),
            &evidence,
        );
        assert_eq!(first, second);
        assert_eq!(first.options.len(), 1);
        assert_eq!(first.options[0].selection.model_id, "gpt-5");
        assert_eq!(first.options[0].reasoning_efforts[0].value, "high");
        assert_eq!(first.options[0].modes[0].value, "build");
        assert_eq!(first.options[0].features.len(), 2);
        assert!(
            first.options[0]
                .features
                .iter()
                .any(|feature| feature.id == "auto_apply"
                    && feature.kind == SessionRuntimeFeatureKind::Toggle)
        );
        assert!(
            first.options[0]
                .features
                .iter()
                .any(|feature| feature.id == CANONICAL_APPROVAL_MODE
                    && feature.kind == SessionRuntimeFeatureKind::Select)
        );
        assert_eq!(
            first.options[0]
                .selection
                .config_values
                .get("auto_apply")
                .map(String::as_str),
            Some("true")
        );
        assert!(
            first.options[0]
                .features
                .iter()
                .all(|feature| feature.id != "model" && feature.id != "thinking_level")
        );
        let mut changed_evidence = evidence.clone();
        changed_evidence
            .values_mut()
            .next()
            .unwrap()
            .options
            .iter_mut()
            .find(|option| option.id == "auto-apply")
            .unwrap()
            .current_value = Some(ProviderSessionConfigValue {
            value: "false".to_string(),
            label: None,
        });
        let changed = build_runtime_option_catalog(&[agent], &[profile], &changed_evidence);
        assert_ne!(first.revision, changed.revision);
        let json = serde_json::to_string(&first).unwrap();
        for forbidden in ["adapter", "nativeSession", "accountAlias", "secret"] {
            assert!(!json.contains(forbidden), "catalog leaked {forbidden}");
        }
    }

    #[test]
    fn runtime_option_catalog_does_not_fabricate_effort_or_include_disabled_rows() {
        let definitions = builtin_agent_definitions();
        let definition = definitions
            .iter()
            .find(|definition| definition.id.as_str() == "claude")
            .unwrap();
        let mut agent = AgentSnapshotEntry::from_definition(definition, None, None);
        agent.added = true;
        agent.enabled = true;
        agent.config_status = AgentConfigStatus::Configured;
        agent.runtime_status = AgentRuntimeStatus::Ready;
        let profile = ProviderProfileSummary {
            id: vibex_core::ProviderProfileId::new(),
            agent_id: agent.id.clone(),
            kind: ProviderKind::Acp,
            display_name: "Claude".to_string(),
            status: ProviderProfileStatus::Enabled,
            account_alias: None,
            default_model: None,
            configured_models: vec![ProviderConfiguredModel {
                id: "sonnet".to_string(),
                display_name: None,
                enabled: true,
                wire_api: None,
            }],
            secret_setup_state: ProviderSecretSetupState::Available,
            updated_at_ms: 1,
        };
        let catalog = build_runtime_option_catalog(
            &[agent.clone()],
            std::slice::from_ref(&profile),
            &BTreeMap::new(),
        );
        assert_eq!(catalog.options.len(), 1);
        assert!(catalog.options[0].reasoning_efforts.is_empty());
        assert!(catalog.options[0].modes.is_empty());

        let mut native_config_profile = profile.clone();
        native_config_profile.id = vibex_core::ProviderProfileId::new();
        native_config_profile.kind = ProviderKind::Claude;
        let catalog = build_runtime_option_catalog(
            &[agent.clone()],
            &[profile.clone(), native_config_profile],
            &BTreeMap::new(),
        );
        assert_eq!(catalog.options.len(), 1);

        agent.enabled = false;
        assert!(
            build_runtime_option_catalog(&[agent], &[profile], &BTreeMap::new())
                .options
                .is_empty()
        );
    }

    #[test]
    fn runtime_option_catalog_uses_probe_models_only_without_explicit_configuration() {
        let definitions = builtin_agent_definitions();
        let definition = definitions
            .iter()
            .find(|definition| definition.id.as_str() == "claude")
            .unwrap();
        let mut agent = AgentSnapshotEntry::from_definition(definition, None, None);
        agent.added = true;
        agent.enabled = true;
        agent.config_status = AgentConfigStatus::Configured;
        agent.runtime_status = AgentRuntimeStatus::Ready;
        let profile = ProviderProfileSummary {
            id: vibex_core::ProviderProfileId::new(),
            agent_id: agent.id.clone(),
            kind: ProviderKind::Acp,
            display_name: "Claude".to_string(),
            status: ProviderProfileStatus::Enabled,
            account_alias: None,
            default_model: None,
            configured_models: Vec::new(),
            secret_setup_state: ProviderSecretSetupState::Available,
            updated_at_ms: 1,
        };
        let evidence = BTreeMap::from([(
            profile.id.clone(),
            RuntimeOptionCatalogProfileEvidence {
                models: vec![SessionModelCatalogEntry {
                    model_id: "sonnet".to_string(),
                    reasoning_efforts: vec![AgentReasoningEffort {
                        value: "high".to_string(),
                        description: Some("High".to_string()),
                    }],
                    default_reasoning_effort: Some("high".to_string()),
                    source: SessionModelCatalogSource::Probe,
                }],
                modes: vec![ProviderSessionConfigValue {
                    value: "plan".to_string(),
                    label: Some("Plan".to_string()),
                }],
                options: vec![catalog_option(
                    "fast-mode",
                    Some("model_config"),
                    ProviderSessionConfigOptionKind::Select,
                    "off",
                    &[("off", "Off"), ("on", "On")],
                )],
                temporarily_unavailable: false,
            },
        )]);

        let catalog = build_runtime_option_catalog(
            std::slice::from_ref(&agent),
            std::slice::from_ref(&profile),
            &evidence,
        );
        assert_eq!(catalog.options.len(), 1);
        assert_eq!(catalog.options[0].selection.model_id, "sonnet");
        assert_eq!(catalog.options[0].reasoning_efforts[0].value, "high");
        assert_eq!(catalog.options[0].modes[0].value, "plan");
        assert_eq!(catalog.options[0].features[0].id, "fast_mode");

        let mut explicitly_disabled = profile;
        explicitly_disabled.configured_models = vec![ProviderConfiguredModel {
            id: "sonnet".to_string(),
            display_name: None,
            enabled: false,
            wire_api: None,
        }];
        assert!(
            build_runtime_option_catalog(&[agent], &[explicitly_disabled], &evidence)
                .options
                .is_empty()
        );
    }
}

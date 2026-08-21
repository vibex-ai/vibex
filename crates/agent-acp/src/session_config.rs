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
    AgentAuthContext, AgentAuthContextStatus, AgentAuthModelCatalogSnapshot,
    AgentAuthModelCatalogStatus, AgentConfigStatus, AgentId, AgentReasoningEffort,
    AgentRuntimeStatus, AgentSnapshotEntry, ProviderKind, ProviderProfileStatus,
    ProviderProfileSummary, ProviderSessionConfigOption, ProviderSessionConfigOptionKind,
    ProviderSessionConfigValue, RuntimeAgentSummary, RuntimeAuthSource, RuntimeAuthSourceAction,
    RuntimeAuthSourceAvailability, RuntimeAuthSourceKind, RuntimeAuthSourceSummary,
    RuntimeModelSelection, RuntimeOptionAvailability, SessionConfigValue, SessionRuntimeFeature,
    SessionRuntimeFeatureKind, SessionRuntimeOption, SessionRuntimeOptionCatalog,
    SessionRuntimeSelection,
};

use crate::protocol::{AcpOperation, AcpOperationStability, AcpWireEncoding, CapabilitySource};
use crate::registry::{CapabilitySupport, fallback_session_modes};

pub const CANONICAL_MODEL: &str = "model";
pub const CANONICAL_REASONING_EFFORT: &str = "reasoning_effort";
pub const CANONICAL_APPROVAL_MODE: &str = "approval_mode";
pub const CANONICAL_SANDBOX_MODE: &str = "sandbox_mode";
const MAX_CANONICAL_KEY_LEN: usize = 80;
const MAX_MODEL_ID_LEN: usize = 160;
const MAX_EFFORT_VALUE_LEN: usize = 64;
const MAX_CATALOG_LABEL_LEN: usize = 160;
const RUNTIME_OPTION_CATALOG_DOMAIN: &[u8] = b"vibex/runtime-option-catalog/v2";
const REASONING_EFFORT_MODE_VALUES: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

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
/// alias still has to be explicitly registered by the adapter compatibility
/// descriptor for the active version boundary.
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

/// Resolves an Agent option id using explicit version-compatible aliases.  The
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
    reasoning_effort_mode_bridge: bool,
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
            reasoning_effort_mode_bridge: false,
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

    /// Replaces the complete option set advertised for the current session.
    /// ACP `ConfigOptionUpdate` notifications carry a full snapshot, so the
    /// planner must not retain options that the Agent has withdrawn.
    pub fn with_options(&self, options: Vec<ProviderSessionConfigOption>) -> Self {
        let mut next = self.clone();
        next.options = options;
        next.add_reasoning_effort_mode_operation();
        next
    }

    /// Enables the provider-specific bridge used by Grok's xAI session
    /// metadata, where each reasoning level is a separate `mode` option.
    pub fn with_reasoning_effort_mode_bridge(mut self) -> Self {
        self.reasoning_effort_mode_bridge = true;
        self.add_reasoning_effort_mode_operation();
        self
    }

    fn add_reasoning_effort_mode_operation(&mut self) {
        if !self.reasoning_effort_mode_bridge {
            return;
        }
        add_reasoning_effort_mode_operation(
            &mut self.operations,
            &self.compatibility_identity,
            self.activation_generation,
            &self.options,
        );
    }

    /// Grok exposes reasoning levels as individual xAI `mode` options rather
    /// than as one selectable `reasoning_effort` option. Those values still
    /// use ACP's typed `session/set_mode` request on the wire.
    pub fn reasoning_effort_value_is_advertised(&self, value: &str) -> bool {
        if self.reasoning_effort_mode_bridge
            && reasoning_effort_mode_value_is_advertised(&self.options, value)
        {
            return true;
        }
        let Ok(key) = CanonicalSessionConfigKey::parse(CANONICAL_REASONING_EFFORT) else {
            return false;
        };
        self.option_for_key(&key)
            .ok()
            .flatten()
            .is_some_and(|option| match option.kind {
                ProviderSessionConfigOptionKind::Boolean => matches!(value, "true" | "false"),
                ProviderSessionConfigOptionKind::Select => {
                    option.values.is_empty() || option.values.iter().any(|item| item.value == value)
                }
                ProviderSessionConfigOptionKind::String => {
                    !value.trim().is_empty() && value.len() <= 256
                }
            })
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

    /// Resolves a complete advertised option. A registered semantic category
    /// may identify a future option id, while two conflicting semantic claims
    /// fail closed instead of silently preferring either field.
    pub fn option_key_for_option(
        &self,
        option: &ProviderSessionConfigOption,
    ) -> Result<CanonicalSessionConfigKey, CanonicalKeyError> {
        let id_key = self.option_key(&option.id)?;
        let Some(category) = option.category.as_deref() else {
            return Ok(id_key);
        };
        let category_key = self.option_key(category)?;
        let id_is_semantic = self.is_registered_semantic_key(&id_key);
        let category_is_semantic = self.is_registered_semantic_key(&category_key);
        if id_is_semantic && category_is_semantic && id_key != category_key {
            return Err(CanonicalKeyError::Ambiguous(
                BTreeSet::from([id_key, category_key])
                    .into_iter()
                    .map(|key| key.to_string())
                    .collect(),
            ));
        }
        if category_is_semantic {
            // ACP categories may group several controls rather than identify
            // one control. Cline, for example, advertises both `provider` and
            // `model` in the `model` category. Prefer the explicitly named
            // semantic option and use the category only as a compatibility
            // fallback when no option id claims that semantic key.
            if !id_is_semantic && self.has_explicit_option_for_key(&category_key)? {
                return Ok(id_key);
            }
            return Ok(category_key);
        }
        Ok(id_key)
    }

    fn has_explicit_option_for_key(
        &self,
        key: &CanonicalSessionConfigKey,
    ) -> Result<bool, CanonicalKeyError> {
        for option in &self.options {
            let option_key = self.option_key(&option.id)?;
            if self.is_registered_semantic_key(&option_key) && option_key == *key {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn is_registered_semantic_key(&self, key: &CanonicalSessionConfigKey) -> bool {
        key.is_reserved()
            || key.as_str() == "mode"
            || self
                .aliases
                .keys()
                .any(|canonical| normalize_identifier(canonical) == key.as_str())
    }

    pub fn option_for_key(
        &self,
        key: &CanonicalSessionConfigKey,
    ) -> Result<Option<&ProviderSessionConfigOption>, CanonicalKeyError> {
        let mut found = None;
        for option in &self.options {
            if self.option_key_for_option(option)? == *key {
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
            SessionConfigFieldKind::ReasoningEffort
                if self.reasoning_effort_mode_bridge
                    && reasoning_effort_mode_value_is_advertised(&self.options, &field.value) =>
            {
                self.plan_reasoning_effort_via_mode()
            }
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
            SessionConfigFieldKind::ReasoningEffort
                if self.reasoning_effort_mode_bridge
                    && reasoning_effort_mode_value_is_advertised(&self.options, &field.value) =>
            {
                Some(AcpOperation::SessionSetMode)
            }
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

    fn plan_reasoning_effort_via_mode(&self) -> SessionConfigPlan {
        self.supported_operation_candidates(&AcpOperation::SessionSetMode)
            .into_iter()
            .find(|evidence| {
                matches!(
                    evidence.encoding,
                    AcpWireEncoding::Typed | AcpWireEncoding::VersionedRaw
                )
            })
            .map(|evidence| SessionConfigPlan::Live {
                operation: AcpOperation::SessionSetMode,
                encoding: evidence.encoding,
                source: evidence.source,
                option_id: None,
            })
            .unwrap_or(SessionConfigPlan::Unavailable)
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

fn reasoning_effort_mode_value_is_advertised(
    options: &[ProviderSessionConfigOption],
    value: &str,
) -> bool {
    let normalized = normalize_identifier(value);
    if !REASONING_EFFORT_MODE_VALUES.contains(&normalized.as_str()) {
        return false;
    }
    options.iter().any(|option| {
        normalize_identifier(option.category.as_deref().unwrap_or_default()) == "mode"
            && normalize_identifier(&option.id) == normalized
            && option.values.is_empty()
    }) && reasoning_effort_mode_option_set(options)
}

fn reasoning_effort_mode_option_set(options: &[ProviderSessionConfigOption]) -> bool {
    let mode_options = options
        .iter()
        .filter(|option| {
            normalize_identifier(option.category.as_deref().unwrap_or_default()) == "mode"
        })
        .collect::<Vec<_>>();
    mode_options.len() >= 2
        && mode_options.iter().all(|option| {
            option.values.is_empty()
                && REASONING_EFFORT_MODE_VALUES.contains(&normalize_identifier(&option.id).as_str())
        })
}

fn add_reasoning_effort_mode_operation(
    operations: &mut BTreeMap<AcpOperation, SessionConfigOperationEvidence>,
    compatibility_identity: &str,
    activation_generation: i64,
    options: &[ProviderSessionConfigOption],
) {
    if !reasoning_effort_mode_option_set(options) {
        return;
    }
    operations
        .entry(AcpOperation::SessionSetMode)
        .or_insert(SessionConfigOperationEvidence {
            support: CapabilitySupport::Supported,
            source: CapabilitySource::NegotiatedRuntime,
            encoding: AcpWireEncoding::Typed,
            stability: AcpOperationStability::CapabilityGated,
            compatibility_identity: compatibility_identity.to_string(),
            activation_generation,
        });
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
    #[serde(default)]
    pub modes: Vec<ProviderSessionConfigValue>,
    #[serde(default)]
    pub options: Vec<ProviderSessionConfigOption>,
    #[serde(default)]
    pub runtime_options_complete: bool,
    pub source: SessionModelCatalogSource,
}

/// Capability evidence scoped to one Provider Profile. It contains only
/// product-safe values: callers keep adapter identity, raw payloads and native
/// ids outside the Runtime Option Catalog boundary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeOptionCatalogProfileEvidence {
    pub models: Vec<SessionModelCatalogEntry>,
    pub modes: Vec<ProviderSessionConfigValue>,
    pub reasoning_efforts: Vec<AgentReasoningEffort>,
    pub options: Vec<ProviderSessionConfigOption>,
    pub temporarily_unavailable: bool,
}

/// Runtime option evidence owned by one Agent CLI. It intentionally contains
/// no Provider Profile or model identity; profiles only supply the model
/// choices that are rendered alongside this shared Agent capability set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeOptionCatalogAgentEvidence {
    pub modes: Vec<ProviderSessionConfigValue>,
    pub reasoning_efforts: Vec<AgentReasoningEffort>,
    pub options: Vec<ProviderSessionConfigOption>,
    pub temporarily_unavailable: bool,
}

/// Builds the runtime option catalog from one persisted capability snapshot
/// per Agent. Provider Profiles contribute only their configured model IDs;
/// no profile config is consulted for modes, efforts, or Features.
pub fn build_runtime_option_catalog_for_agents(
    agents: &[AgentSnapshotEntry],
    profiles: &[ProviderProfileSummary],
    evidence_by_agent: &BTreeMap<AgentId, RuntimeOptionCatalogAgentEvidence>,
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
        let evidence = evidence_by_agent.get(&agent.id);
        let profile_evidence = evidence.map(|entry| RuntimeOptionCatalogProfileEvidence {
            models: Vec::new(),
            modes: entry.modes.clone(),
            reasoning_efforts: entry.reasoning_efforts.clone(),
            options: entry.options.clone(),
            temporarily_unavailable: entry.temporarily_unavailable,
        });
        let reasoning_efforts = evidence
            .map(|entry| entry.reasoning_efforts.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|effort| {
                let value = validate_effort_value(&effort.value).ok()?;
                Some(SessionConfigValue {
                    value,
                    label: effort.description.as_deref().map(bounded_catalog_label),
                })
            })
            .collect::<Vec<_>>();
        let modes = catalog_modes_from_values(
            evidence
                .map(|entry| entry.modes.clone())
                .unwrap_or_default(),
            &agent.id,
        );
        let features = catalog_features(profile_evidence.as_ref());
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

        let mut model_ids = profile
            .configured_models
            .iter()
            .filter(|model| model.enabled)
            .filter_map(|model| {
                let id = validate_model_value(&model.id).ok()?;
                let label = bounded_catalog_label(model.display_name.as_deref().unwrap_or(&id));
                Some((id, label))
            })
            .collect::<BTreeMap<_, _>>();
        if model_ids.is_empty()
            && let Some(model) = profile
                .default_model
                .as_deref()
                .and_then(|model| validate_model_value(model).ok())
        {
            model_ids.insert(model.clone(), model);
        }
        let availability = catalog_availability(
            agent,
            profile_evidence.as_ref(),
            !profile.configured_models.is_empty()
                || profile
                    .default_model
                    .as_deref()
                    .is_some_and(|model| !model.trim().is_empty()),
        );
        for (model_id, model_label) in model_ids {
            let mut selection =
                SessionRuntimeSelection::provider(agent.id.clone(), profile.id.clone(), model_id);
            selection.config_values = feature_config_values.clone();
            options.push(SessionRuntimeOption {
                selection,
                agent_label: bounded_catalog_label(&agent.label),
                auth_source_label: bounded_catalog_label(&profile.display_name),
                model_label,
                reasoning_efforts: reasoning_efforts.clone(),
                modes: modes.clone(),
                features: features.clone(),
                availability,
            });
        }
    }

    let mut catalog = SessionRuntimeOptionCatalog {
        revision: 1,
        agents: catalog_agent_summaries(agents),
        auth_sources: provider_auth_source_summaries(agents, profiles),
        options,
    };
    refresh_runtime_option_catalog_revision(&mut catalog, std::iter::empty::<&[u8]>());
    catalog
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

        let availability = catalog_availability(agent, evidence, has_explicit_model_configuration);
        let fallback_modes = catalog_modes(evidence);
        let fallback_features = catalog_features(evidence);
        for (model_id, model_label) in configured_models {
            let model_evidence = evidence_models.get(&model_id).copied();
            let modes = model_evidence
                .filter(|model| model.runtime_options_complete || !model.modes.is_empty())
                .map(|model| {
                    catalog_modes(Some(&RuntimeOptionCatalogProfileEvidence {
                        modes: model.modes.clone(),
                        ..Default::default()
                    }))
                })
                .unwrap_or_else(|| fallback_modes.clone());
            let features = model_evidence
                .filter(|model| model.runtime_options_complete || !model.options.is_empty())
                .map(|model| {
                    catalog_features(Some(&RuntimeOptionCatalogProfileEvidence {
                        options: model.options.clone(),
                        ..Default::default()
                    }))
                })
                .unwrap_or_else(|| fallback_features.clone());
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
            let mut selection =
                SessionRuntimeSelection::provider(agent.id.clone(), profile.id.clone(), model_id);
            selection.config_values = feature_config_values;
            options.push(SessionRuntimeOption {
                selection,
                agent_label: bounded_catalog_label(&agent.label),
                auth_source_label: bounded_catalog_label(&profile.display_name),
                model_label,
                reasoning_efforts: model_evidence
                    .filter(|model| {
                        model.runtime_options_complete || !model.reasoning_efforts.is_empty()
                    })
                    .map(catalog_reasoning_efforts)
                    .unwrap_or_else(|| {
                        catalog_reasoning_effort_values(
                            evidence
                                .map(|evidence| evidence.reasoning_efforts.as_slice())
                                .unwrap_or_default(),
                        )
                    }),
                modes,
                features,
                availability,
            });
        }
    }

    let mut catalog = SessionRuntimeOptionCatalog {
        revision: 1,
        agents: catalog_agent_summaries(agents),
        auth_sources: provider_auth_source_summaries(agents, profiles),
        options,
    };
    refresh_runtime_option_catalog_revision(&mut catalog, std::iter::empty::<&[u8]>());
    catalog
}

/// Adds the one default account owned by an Agent to an existing Provider
/// catalog. Account sources remain visible while signed out; executable model
/// options are emitted only from evidence for the current context revision.
pub fn append_agent_account_runtime_options(
    catalog: &mut SessionRuntimeOptionCatalog,
    agent: &AgentSnapshotEntry,
    context: &AgentAuthContext,
    snapshot: Option<&AgentAuthModelCatalogSnapshot>,
    supports_logout: bool,
) {
    let model_catalog_status = snapshot
        .map(|snapshot| snapshot.status)
        .unwrap_or(AgentAuthModelCatalogStatus::Unknown);
    let availability = agent_account_availability(agent, context, model_catalog_status);
    let mut supported_actions = Vec::new();
    if context.status != AgentAuthContextStatus::Verifying {
        supported_actions.push(RuntimeAuthSourceAction::Authenticate);
        supported_actions.push(RuntimeAuthSourceAction::Verify);
    }
    if context.status == AgentAuthContextStatus::Authenticated {
        supported_actions.push(RuntimeAuthSourceAction::RefreshModels);
        if supports_logout {
            supported_actions.push(RuntimeAuthSourceAction::Logout);
        }
    }
    let source = RuntimeAuthSource::agent_account(context.id.clone());
    catalog.auth_sources.push(RuntimeAuthSourceSummary {
        source: source.clone(),
        auth_source_revision: context.revision,
        agent_id: agent.id.clone(),
        label: "Default CLI account".to_string(),
        kind: RuntimeAuthSourceKind::AgentAccount,
        availability,
        account_hint: context.account_hint.as_deref().map(bounded_catalog_label),
        model_catalog_status,
        supported_actions,
    });
    if availability != RuntimeAuthSourceAvailability::Available {
        return;
    }

    match snapshot.map(|snapshot| snapshot.status) {
        Some(AgentAuthModelCatalogStatus::Available) => {
            for model in snapshot
                .into_iter()
                .flat_map(|snapshot| snapshot.models.iter())
            {
                let Ok(model_id) = validate_model_value(&model.model_id) else {
                    continue;
                };
                let reasoning_efforts = model.reasoning_efforts.clone();
                let modes = model.modes.clone();
                let features = model.features.clone();
                let mut selection =
                    SessionRuntimeSelection::agent_default(agent.id.clone(), context.id.clone());
                selection.model = RuntimeModelSelection::explicit(model_id);
                selection.config_values = default_feature_values(&features);
                catalog.options.push(SessionRuntimeOption {
                    selection,
                    agent_label: bounded_catalog_label(&agent.label),
                    auth_source_label: "Default CLI account".to_string(),
                    model_label: bounded_catalog_label(&model.label),
                    reasoning_efforts,
                    modes,
                    features,
                    availability: RuntimeOptionAvailability::Available,
                });
            }
        }
        Some(AgentAuthModelCatalogStatus::AgentDefaultOnly) => {
            let snapshot = snapshot.expect("Agent-default status requires a snapshot");
            let mut selection =
                SessionRuntimeSelection::agent_default(agent.id.clone(), context.id.clone());
            selection.config_values = default_feature_values(&snapshot.default_features);
            catalog.options.push(SessionRuntimeOption {
                selection,
                agent_label: bounded_catalog_label(&agent.label),
                auth_source_label: "Default CLI account".to_string(),
                model_label: "Selected automatically by Agent".to_string(),
                reasoning_efforts: snapshot.default_reasoning_efforts.clone(),
                modes: snapshot.default_modes.clone(),
                features: snapshot.default_features.clone(),
                availability: RuntimeOptionAvailability::Available,
            });
        }
        _ => {}
    }
}

fn default_feature_values(features: &[SessionRuntimeFeature]) -> BTreeMap<String, String> {
    features
        .iter()
        .filter_map(|feature| {
            feature
                .current_value
                .as_ref()
                .or(feature.default_value.as_ref())
                .map(|value| (feature.id.clone(), value.value.clone()))
        })
        .collect()
}

fn agent_account_availability(
    agent: &AgentSnapshotEntry,
    context: &AgentAuthContext,
    model_catalog_status: AgentAuthModelCatalogStatus,
) -> RuntimeAuthSourceAvailability {
    if !agent.installed {
        return RuntimeAuthSourceAvailability::Unsupported;
    }
    if matches!(
        agent.runtime_status,
        AgentRuntimeStatus::Unavailable | AgentRuntimeStatus::ProbeFailed
    ) {
        return RuntimeAuthSourceAvailability::TemporarilyUnavailable;
    }
    match context.status {
        AgentAuthContextStatus::Unverified | AgentAuthContextStatus::AuthenticationRequired => {
            RuntimeAuthSourceAvailability::RequiresAuthentication
        }
        AgentAuthContextStatus::Verifying => RuntimeAuthSourceAvailability::Verifying,
        AgentAuthContextStatus::Unavailable => {
            RuntimeAuthSourceAvailability::TemporarilyUnavailable
        }
        AgentAuthContextStatus::Authenticated => match model_catalog_status {
            AgentAuthModelCatalogStatus::Available
            | AgentAuthModelCatalogStatus::AgentDefaultOnly => {
                RuntimeAuthSourceAvailability::Available
            }
            AgentAuthModelCatalogStatus::AuthenticationRequired => {
                RuntimeAuthSourceAvailability::RequiresAuthentication
            }
            AgentAuthModelCatalogStatus::Unavailable => {
                RuntimeAuthSourceAvailability::TemporarilyUnavailable
            }
            AgentAuthModelCatalogStatus::Unknown | AgentAuthModelCatalogStatus::Discovering => {
                RuntimeAuthSourceAvailability::DiscoveringModels
            }
        },
    }
}

fn catalog_agent_summaries(agents: &[AgentSnapshotEntry]) -> Vec<RuntimeAgentSummary> {
    let mut enabled = agents
        .iter()
        .filter(|agent| agent.added && agent.enabled)
        .collect::<Vec<_>>();
    enabled.sort_by(|left, right| {
        left.order_index
            .cmp(&right.order_index)
            .then_with(|| left.id.cmp(&right.id))
    });
    enabled
        .into_iter()
        .map(|agent| RuntimeAgentSummary {
            agent_id: agent.id.clone(),
            label: bounded_catalog_label(&agent.label),
        })
        .collect()
}

fn provider_auth_source_summaries(
    agents: &[AgentSnapshotEntry],
    profiles: &[ProviderProfileSummary],
) -> Vec<RuntimeAuthSourceSummary> {
    let agents_by_id = agents
        .iter()
        .filter(|agent| agent.added && agent.enabled)
        .map(|agent| (agent.id.clone(), agent))
        .collect::<BTreeMap<_, _>>();
    let mut summaries = profiles
        .iter()
        .filter(|profile| {
            profile.kind == ProviderKind::Acp && profile.status == ProviderProfileStatus::Enabled
        })
        .filter_map(|profile| {
            let agent = agents_by_id.get(&profile.agent_id)?;
            Some((
                agent.order_index,
                RuntimeAuthSourceSummary {
                    source: RuntimeAuthSource::provider_profile(profile.id.clone()),
                    auth_source_revision: profile.updated_at_ms,
                    agent_id: profile.agent_id.clone(),
                    label: bounded_catalog_label(&profile.display_name),
                    kind: RuntimeAuthSourceKind::ProviderProfile,
                    availability: if matches!(
                        agent.config_status,
                        AgentConfigStatus::NeedsConfiguration | AgentConfigStatus::Unknown
                    ) {
                        RuntimeAuthSourceAvailability::RequiresConfiguration
                    } else {
                        RuntimeAuthSourceAvailability::Available
                    },
                    account_hint: None,
                    model_catalog_status: if profile.configured_models.is_empty()
                        && profile
                            .default_model
                            .as_deref()
                            .is_none_or(|model| model.trim().is_empty())
                    {
                        AgentAuthModelCatalogStatus::Unknown
                    } else {
                        AgentAuthModelCatalogStatus::Available
                    },
                    supported_actions: vec![RuntimeAuthSourceAction::ConfigureProvider],
                },
            ))
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|(left_order, left), (right_order, right)| {
        left_order
            .cmp(right_order)
            .then_with(|| left.agent_id.cmp(&right.agent_id))
            .then_with(|| left.label.cmp(&right.label))
            .then_with(|| left.source.id().cmp(right.source.id()))
    });
    summaries.into_iter().map(|(_, summary)| summary).collect()
}

fn catalog_availability(
    agent: &AgentSnapshotEntry,
    evidence: Option<&RuntimeOptionCatalogProfileEvidence>,
    has_explicit_model_configuration: bool,
) -> RuntimeOptionAvailability {
    if matches!(
        agent.config_status,
        AgentConfigStatus::NeedsConfiguration | AgentConfigStatus::Unknown
    ) {
        RuntimeOptionAvailability::RequiresConfiguration
    } else if (!has_explicit_model_configuration
        && evidence.is_some_and(|value| value.temporarily_unavailable))
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

fn catalog_modes_from_values(
    values: Vec<ProviderSessionConfigValue>,
    agent_id: &AgentId,
) -> Vec<SessionConfigValue> {
    let mut modes = values
        .into_iter()
        .filter_map(|mode| {
            let value = validate_effort_value(&mode.value).ok()?;
            Some(SessionConfigValue {
                value,
                label: mode.label.as_deref().map(bounded_catalog_label),
            })
        })
        .collect::<Vec<_>>();
    if modes.is_empty() {
        modes = fallback_session_modes(agent_id)
            .into_iter()
            .filter_map(|mode| {
                let value = validate_effort_value(&mode.value).ok()?;
                Some(SessionConfigValue {
                    value,
                    label: mode.label.as_deref().map(bounded_catalog_label),
                })
            })
            .collect();
    }
    modes.sort_by(|left, right| left.value.cmp(&right.value));
    modes.dedup_by(|left, right| left.value == right.value);
    modes
}

fn catalog_reasoning_efforts(model: &SessionModelCatalogEntry) -> Vec<SessionConfigValue> {
    catalog_reasoning_effort_values(&model.reasoning_efforts)
}

fn catalog_reasoning_effort_values(
    reasoning_efforts: &[AgentReasoningEffort],
) -> Vec<SessionConfigValue> {
    let mut efforts = reasoning_efforts
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

pub(crate) fn runtime_features_from_options(
    options: &[ProviderSessionConfigOption],
) -> Vec<SessionRuntimeFeature> {
    catalog_features(Some(&RuntimeOptionCatalogProfileEvidence {
        options: options.to_vec(),
        ..Default::default()
    }))
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

/// Recomputes the deterministic stale-selection revision after callers merge
/// additional authentication sources into the catalog. Extra components are
/// reserved for safe internal identities such as a model-catalog fingerprint.
pub fn refresh_runtime_option_catalog_revision<'a>(
    catalog: &mut SessionRuntimeOptionCatalog,
    extra_components: impl IntoIterator<Item = &'a [u8]>,
) {
    let mut hasher = Sha256::new();
    write_catalog_component(&mut hasher, RUNTIME_OPTION_CATALOG_DOMAIN);
    for agent in &catalog.agents {
        write_catalog_component(&mut hasher, b"agent");
        write_catalog_component(&mut hasher, agent.agent_id.as_str().as_bytes());
        write_catalog_component(&mut hasher, agent.label.as_bytes());
    }
    for source in &catalog.auth_sources {
        write_catalog_component(&mut hasher, b"auth_source");
        write_catalog_component(&mut hasher, source.agent_id.as_str().as_bytes());
        write_catalog_component(
            &mut hasher,
            match source.kind {
                RuntimeAuthSourceKind::ProviderProfile => b"provider_profile",
                RuntimeAuthSourceKind::AgentAccount => b"agent_account",
            },
        );
        write_catalog_component(&mut hasher, source.source.id().as_bytes());
        write_catalog_component(&mut hasher, &source.auth_source_revision.to_be_bytes());
        write_catalog_component(&mut hasher, source.label.as_bytes());
        write_catalog_component(
            &mut hasher,
            source
                .account_hint
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        write_catalog_component(&mut hasher, format!("{:?}", source.availability).as_bytes());
        write_catalog_component(
            &mut hasher,
            format!("{:?}", source.model_catalog_status).as_bytes(),
        );
        for action in &source.supported_actions {
            write_catalog_component(&mut hasher, format!("{action:?}").as_bytes());
        }
    }
    for option in &catalog.options {
        write_catalog_component(&mut hasher, option.selection.agent_id.as_str().as_bytes());
        write_catalog_component(
            &mut hasher,
            match option.selection.auth_source.kind() {
                RuntimeAuthSourceKind::ProviderProfile => b"provider_profile",
                RuntimeAuthSourceKind::AgentAccount => b"agent_account",
            },
        );
        write_catalog_component(&mut hasher, option.selection.auth_source.id().as_bytes());
        write_catalog_component(
            &mut hasher,
            option.selection.model_id().unwrap_or_default().as_bytes(),
        );
        write_catalog_component(&mut hasher, option.agent_label.as_bytes());
        write_catalog_component(&mut hasher, option.auth_source_label.as_bytes());
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
    for component in extra_components {
        write_catalog_component(&mut hasher, b"extra");
        write_catalog_component(&mut hasher, component);
    }
    let digest = hasher.finalize();
    let mut revision = [0_u8; 8];
    revision.copy_from_slice(&digest[..8]);
    let revision = i64::from_be_bytes(revision) & i64::MAX;
    catalog.revision = if revision == 0 { 1 } else { revision };
}

fn write_catalog_component(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

/// Merges model evidence by source priority. Runtime values are attached only
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
                    modes: Vec::new(),
                    options: Vec::new(),
                    runtime_options_complete: false,
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
                Some(_) => {
                    let existing = merged
                        .get_mut(&entry.model_id)
                        .expect("catalog entry existence checked above");
                    if !existing.runtime_options_complete {
                        if existing.reasoning_efforts.is_empty()
                            && !entry.reasoning_efforts.is_empty()
                        {
                            existing.reasoning_efforts = entry.reasoning_efforts;
                            existing.default_reasoning_effort = entry.default_reasoning_effort;
                        }
                        if existing.modes.is_empty() && !entry.modes.is_empty() {
                            existing.modes = entry.modes;
                        }
                        if existing.options.is_empty() && !entry.options.is_empty() {
                            existing.options = entry.options;
                        }
                        existing.runtime_options_complete = entry.runtime_options_complete;
                    }
                }
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
        AgentAuthContextId, AgentModelDiscoverySource, ProviderConfiguredModel, ProviderKind,
        ProviderProfileStatus, ProviderSecretSetupState, ProviderSessionConfigOptionKind,
        ProviderSessionConfigValue, builtin_agent_definitions,
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
    fn agent_account_catalog_exposes_one_source_and_real_agent_default_selection() {
        let definition = builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.id.as_str() == "codex")
            .unwrap();
        let mut agent = AgentSnapshotEntry::from_definition(&definition, None, None);
        agent.added = true;
        agent.enabled = true;
        agent.installed = true;
        agent.configured = true;
        agent.config_status = AgentConfigStatus::Configured;
        agent.runtime_status = AgentRuntimeStatus::Ready;
        let context = AgentAuthContext {
            id: AgentAuthContextId::new(),
            agent_id: agent.id.clone(),
            status: AgentAuthContextStatus::Authenticated,
            account_hint: Some("work account".to_string()),
            authenticated_via_method: Some("browser".to_string()),
            revision: 4,
            last_verified_at_ms: Some(10),
            created_at_ms: 1,
            updated_at_ms: 10,
        };
        let snapshot = AgentAuthModelCatalogSnapshot {
            auth_context_id: context.id.clone(),
            auth_context_revision: context.revision,
            runtime_fingerprint: "codex-runtime-v1".to_string(),
            discovery_source: AgentModelDiscoverySource::AgentDefault,
            status: AgentAuthModelCatalogStatus::AgentDefaultOnly,
            models: Vec::new(),
            runtime_options_complete: true,
            default_reasoning_efforts: vec![SessionConfigValue {
                value: "high".to_string(),
                label: Some("High".to_string()),
            }],
            default_modes: vec![SessionConfigValue {
                value: "build".to_string(),
                label: Some("Build".to_string()),
            }],
            default_features: vec![SessionRuntimeFeature {
                id: "auto_apply".to_string(),
                label: "Auto apply".to_string(),
                description: None,
                kind: SessionRuntimeFeatureKind::Toggle,
                current_value: Some(SessionConfigValue {
                    value: "true".to_string(),
                    label: None,
                }),
                default_value: Some(SessionConfigValue {
                    value: "false".to_string(),
                    label: None,
                }),
                values: Vec::new(),
            }],
            last_success_at_ms: Some(10),
            last_attempt_at_ms: 10,
            last_error_code: None,
        };
        let mut catalog = build_runtime_option_catalog(&[agent.clone()], &[], &BTreeMap::new());

        append_agent_account_runtime_options(&mut catalog, &agent, &context, Some(&snapshot), true);

        assert_eq!(catalog.auth_sources.len(), 1);
        let source = &catalog.auth_sources[0];
        assert_eq!(
            source.source,
            RuntimeAuthSource::agent_account(context.id.clone())
        );
        assert_eq!(source.auth_source_revision, context.revision);
        assert_eq!(
            source.availability,
            RuntimeAuthSourceAvailability::Available
        );
        assert_eq!(source.account_hint.as_deref(), Some("work account"));
        assert!(
            source
                .supported_actions
                .contains(&RuntimeAuthSourceAction::Logout)
        );
        assert_eq!(catalog.options.len(), 1);
        assert_eq!(catalog.options[0].selection.auth_source, source.source);
        assert_eq!(
            catalog.options[0].selection.model,
            RuntimeModelSelection::AgentDefault
        );
        assert_eq!(
            catalog.options[0].model_label,
            "Selected automatically by Agent"
        );
        assert_eq!(catalog.options[0].reasoning_efforts[0].value, "high");
        assert_eq!(catalog.options[0].modes[0].value, "build");
        assert_eq!(catalog.options[0].features[0].id, "auto_apply");
        assert_eq!(
            catalog.options[0]
                .selection
                .config_values
                .get("auto_apply")
                .map(String::as_str),
            Some("true")
        );

        let mut signed_out = context;
        signed_out.status = AgentAuthContextStatus::AuthenticationRequired;
        let mut signed_out_catalog =
            build_runtime_option_catalog(&[agent.clone()], &[], &BTreeMap::new());
        append_agent_account_runtime_options(
            &mut signed_out_catalog,
            &agent,
            &signed_out,
            None,
            true,
        );
        assert_eq!(signed_out_catalog.auth_sources.len(), 1);
        assert_eq!(
            signed_out_catalog.auth_sources[0].availability,
            RuntimeAuthSourceAvailability::RequiresAuthentication
        );
        assert!(signed_out_catalog.options.is_empty());
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
    fn registered_category_maps_future_option_ids_and_conflicts_fail_closed() {
        let aliases = BTreeMap::from([
            (CANONICAL_MODEL.to_string(), vec!["model".to_string()]),
            (
                CANONICAL_REASONING_EFFORT.to_string(),
                vec!["effort".to_string(), "thought_level".to_string()],
            ),
        ]);
        let mut future_effort = option("adaptive_effort_v2", "high");
        future_effort.category = Some("thought_level".to_string());
        let planner = SessionConfigPlanner::new(
            "adapter=test@2",
            1,
            aliases.clone(),
            BTreeMap::new(),
            vec![future_effort],
        );
        let reasoning_key = CanonicalSessionConfigKey::parse(CANONICAL_REASONING_EFFORT).unwrap();
        assert_eq!(
            planner.option_for_key(&reasoning_key).unwrap().unwrap().id,
            "adaptive_effort_v2"
        );

        let mut conflicting = option(CANONICAL_MODEL, "model-1");
        conflicting.category = Some("thought_level".to_string());
        let planner = SessionConfigPlanner::new(
            "adapter=test@2",
            1,
            aliases,
            BTreeMap::new(),
            vec![conflicting],
        );
        assert!(matches!(
            planner.option_for_key(&reasoning_key),
            Err(CanonicalKeyError::Ambiguous(_))
        ));
    }

    #[test]
    fn grok_mode_efforts_use_session_set_mode() {
        let options = ["xhigh", "high", "medium", "low"]
            .into_iter()
            .map(|id| {
                let mut option = option(id, id);
                option.category = Some("mode".to_string());
                option.label = id.to_string();
                option
            })
            .collect();
        let planner = SessionConfigPlanner::new(
            "adapter=test@1",
            1,
            BTreeMap::new(),
            BTreeMap::new(),
            options,
        )
        .with_reasoning_effort_mode_bridge();
        let request = SessionConfigFieldRequest {
            key: CanonicalSessionConfigKey::parse(CANONICAL_REASONING_EFFORT).unwrap(),
            kind: SessionConfigFieldKind::ReasoningEffort,
            value: "low".to_string(),
        };

        assert!(planner.reasoning_effort_value_is_advertised("low"));
        assert!(matches!(
            planner.plan(&request).unwrap(),
            SessionConfigPlan::Live {
                operation: AcpOperation::SessionSetMode,
                encoding: AcpWireEncoding::Typed,
                ..
            }
        ));
    }

    #[test]
    fn explicit_model_option_wins_over_other_controls_in_the_model_category() {
        let aliases = BTreeMap::from([(CANONICAL_MODEL.to_string(), vec!["model".to_string()])]);
        let provider = catalog_option(
            "provider",
            Some(CANONICAL_MODEL),
            ProviderSessionConfigOptionKind::Select,
            "cline",
            &[("cline", "Cline"), ("cline-pass", "ClinePass")],
        );
        let model = catalog_option(
            CANONICAL_MODEL,
            Some(CANONICAL_MODEL),
            ProviderSessionConfigOptionKind::Select,
            "anthropic/claude-sonnet-4",
            &[("anthropic/claude-sonnet-4", "Sonnet")],
        );
        let planner = SessionConfigPlanner::new(
            "adapter=cline@3.0.53",
            1,
            aliases,
            BTreeMap::new(),
            vec![provider, model],
        );
        let model_key = CanonicalSessionConfigKey::parse(CANONICAL_MODEL).unwrap();
        let provider_key = CanonicalSessionConfigKey::parse("provider").unwrap();

        assert_eq!(
            planner.option_for_key(&model_key).unwrap().unwrap().id,
            CANONICAL_MODEL
        );
        assert_eq!(
            planner.option_for_key(&provider_key).unwrap().unwrap().id,
            "provider"
        );
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
                modes: Vec::new(),
                options: Vec::new(),
                runtime_options_complete: false,
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
                modes: Vec::new(),
                options: Vec::new(),
                runtime_options_complete: false,
                source: SessionModelCatalogSource::Extension,
            }],
        );
        assert_eq!(catalog[0].source, SessionModelCatalogSource::Session);
        assert_eq!(catalog[0].reasoning_efforts[0].value, "high");
    }

    #[test]
    fn complete_model_evidence_keeps_empty_runtime_options() {
        let catalog = merge_model_catalog(
            vec![SessionModelCatalogEntry {
                model_id: "model-a".to_string(),
                reasoning_efforts: Vec::new(),
                default_reasoning_effort: None,
                modes: Vec::new(),
                options: Vec::new(),
                runtime_options_complete: true,
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
                modes: Vec::new(),
                options: Vec::new(),
                runtime_options_complete: false,
                source: SessionModelCatalogSource::Extension,
            }],
        );

        assert!(catalog[0].reasoning_efforts.is_empty());
        assert!(catalog[0].runtime_options_complete);
    }

    #[test]
    fn agent_level_runtime_catalog_takes_models_only_from_provider_profiles() {
        let definition = builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.id.as_str() == "claude")
            .unwrap();
        let mut agent = AgentSnapshotEntry::from_definition(&definition, None, None);
        agent.added = true;
        agent.enabled = true;
        agent.config_status = AgentConfigStatus::Configured;
        agent.runtime_status = AgentRuntimeStatus::Ready;
        agent.models = vec!["discovered-agent-model".to_string()];
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

        let catalog =
            build_runtime_option_catalog_for_agents(&[agent], &[profile], &BTreeMap::new());

        assert!(catalog.options.is_empty());
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
                    capabilities: Default::default(),
                },
                ProviderConfiguredModel {
                    id: "disabled".to_string(),
                    display_name: None,
                    enabled: false,
                    wire_api: None,
                    capabilities: Default::default(),
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
                    modes: Vec::new(),
                    options: Vec::new(),
                    runtime_options_complete: false,
                    source: SessionModelCatalogSource::Session,
                }],
                modes: vec![ProviderSessionConfigValue {
                    value: "build".to_string(),
                    label: Some("Build".to_string()),
                }],
                reasoning_efforts: Vec::new(),
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
                // A failed Agent-level probe must not hide an explicitly
                // configured Provider Profile model.
                temporarily_unavailable: true,
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
        assert_eq!(first.options[0].selection.model_id(), Some("gpt-5"));
        assert_eq!(
            first.options[0].availability,
            RuntimeOptionAvailability::Available
        );
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
                capabilities: Default::default(),
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
                    modes: Vec::new(),
                    options: Vec::new(),
                    runtime_options_complete: false,
                    source: SessionModelCatalogSource::Probe,
                }],
                modes: vec![ProviderSessionConfigValue {
                    value: "plan".to_string(),
                    label: Some("Plan".to_string()),
                }],
                reasoning_efforts: Vec::new(),
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
        assert_eq!(catalog.options[0].selection.model_id(), Some("sonnet"));
        assert_eq!(catalog.options[0].reasoning_efforts[0].value, "high");
        assert_eq!(catalog.options[0].modes[0].value, "plan");
        assert_eq!(catalog.options[0].features[0].id, "fast_mode");

        let mut explicitly_disabled = profile;
        explicitly_disabled.configured_models = vec![ProviderConfiguredModel {
            id: "sonnet".to_string(),
            display_name: None,
            enabled: false,
            wire_api: None,
            capabilities: Default::default(),
        }];
        assert!(
            build_runtime_option_catalog(&[agent], &[explicitly_disabled], &evidence)
                .options
                .is_empty()
        );
    }
}

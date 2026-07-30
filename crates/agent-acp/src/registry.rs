//! Exact-version ACP adapter compatibility registry (hot-switch plan §8).
//!
//! The registry is the single source of truth for managed adapter package,
//! command, capability, quirk, and bridge-contract policy. Runtime evidence is
//! still authoritative: descriptors cannot re-enable an operation that a live
//! process has explicitly rejected.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use semver::{Version, VersionReq};
use serde::Serialize;
use vibex_core::{
    AcpAdapterId, AgentId, AgentReasoningEffort, AgentRuntimeRouteKey, ProviderSessionConfigValue,
    TransportKind, VibexError, VibexResult,
};

use crate::protocol::{
    AcpOperation, AcpOperationStability, AcpWireEncoding, CapabilitySource,
    baseline_operation_matrix,
};

pub const CLAUDE_AGENT_ID: &str = "claude";
pub const CLAUDE_ADAPTER_ID: &str = "claude-agent-acp";
pub const CLAUDE_ADAPTER_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";
pub const CLAUDE_ADAPTER_VERSION: &str = "0.58.1";
pub const CLAUDE_ADAPTER_INTEGRITY: &str = "sha512-F1/W6EJdoYbrEUluRUknx0Nn0MAKDOkn2C/9YcP/joVkmdFUGTAxlGDpwdYu239TOkpc8Qm4+ffGsQjPZdryTg==";

pub const CODEX_AGENT_ID: &str = "codex";
pub const CODEX_ADAPTER_ID: &str = "codex-acp";
pub const CODEX_ADAPTER_PACKAGE: &str = "@agentclientprotocol/codex-acp";
pub const CODEX_ADAPTER_VERSION: &str = "1.1.2";
pub const CODEX_ADAPTER_INTEGRITY: &str = "sha512-qE/R1WdqJJ9OFHsHGvbmVmS2j9iCMZzpWT3g2XIViXrGHu1fLOALLINBIlW+WzKDllCh131aB6cqcIWSt0otbw==";
pub const CODEX_RUNTIME_PACKAGE: &str = "@openai/codex";
pub const CODEX_RUNTIME_DECLARED_REQUIREMENT: &str = "^0.144.0";
pub const CODEX_RUNTIME_PIN: &str = "0.144.1";
pub const CODEX_RUNTIME_INTEGRITY: &str = "sha512-Xir1zqPfpenhdoAoshN53uonzbBXj18COyzRkFlVZpSNyEl5XtkuYu9oddELePFN7K/0sXUcSO34Ad5IeCXPbw==";
pub const NPM_REGISTRY_ORIGIN: &str = "https://registry.npmjs.org";

/// Three-state support value used before runtime negotiation is complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySupport {
    Supported,
    Unsupported,
    Unknown,
}

/// Static descriptor policy with a short evidence label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilitySupport {
    pub support: CapabilitySupport,
    pub evidence: String,
}

impl CompatibilitySupport {
    pub fn supported(evidence: impl Into<String>) -> Self {
        Self {
            support: CapabilitySupport::Supported,
            evidence: evidence.into(),
        }
    }

    pub fn unsupported(evidence: impl Into<String>) -> Self {
        Self {
            support: CapabilitySupport::Unsupported,
            evidence: evidence.into(),
        }
    }

    pub fn unknown(evidence: impl Into<String>) -> Self {
        Self {
            support: CapabilitySupport::Unknown,
            evidence: evidence.into(),
        }
    }
}

/// A dependency whose exact installed version participates in runtime identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRuntimeDependency {
    pub package: String,
    pub declared_requirement: VersionReq,
    pub managed_pin: Version,
    pub integrity: String,
    pub include_in_compatibility_identity: bool,
}

/// Managed npm distribution. The package and version are never supplied by UI
/// input; installers consume this descriptor directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAdapterDistribution {
    pub registry_origin: String,
    pub package: String,
    pub exact_version: Version,
    pub integrity: String,
    pub bin_name: String,
    pub initialize_agent_name: String,
    pub node_requirement: VersionReq,
    pub node_requirement_package: String,
    pub runtime_dependencies: Vec<ManagedRuntimeDependency>,
}

impl AcpAdapterDistribution {
    pub fn package_spec(&self) -> String {
        format!("{}@{}", self.package, self.exact_version)
    }
}

/// One fixed way to launch or interrogate the managed adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandVariant {
    pub bin_name: String,
    pub args: Vec<String>,
    pub version_args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestorePolicy {
    LoadThenNew,
    ResumeThenLoadThenNew,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeStateHomePolicy {
    ClaudeConfigDirectory,
    StableCodexHome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptStrategy {
    ClaudeJsonl,
    CodexRollout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentEventEnricherKind {
    Claude,
    Codex,
    Passthrough,
}

/// Exact behavior boundary used by bindings and workaround lookup.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AdapterCompatibilityIdentity(String);

impl AdapterCompatibilityIdentity {
    pub fn new(
        adapter_id: &AcpAdapterId,
        adapter_version: &Version,
        runtime_versions: &BTreeMap<String, Version>,
    ) -> Self {
        let mut value = format!("adapter={}@{}", adapter_id, adapter_version);
        for (package, version) in runtime_versions {
            value.push_str(";runtime=");
            value.push_str(package);
            value.push('@');
            value.push_str(&version.to_string());
        }
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AdapterCompatibilityIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedOperationDescriptor {
    pub operation: AcpOperation,
    pub support: CapabilitySupport,
    pub stability: AcpOperationStability,
    pub encoding: AcpWireEncoding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedAgentQuirk {
    pub id: String,
    pub compatibility_identity: AdapterCompatibilityIdentity,
    pub operation: Option<AcpOperation>,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeContractRequirement {
    Required,
    WhenAdvertised,
    NotApplicable,
}

/// One §25.2 contract case. `operation` is populated when the case maps to a
/// wire operation; permission/MCP/attachments are feature-level cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeContractCase {
    pub id: String,
    pub operation: Option<AcpOperation>,
    pub requirement: BridgeContractRequirement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeContractStatus {
    Passed,
    Failed,
    NotAdvertised,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeContractEvidenceKind {
    RealManagedAdapter,
    Fixture,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeContractCaseResult {
    pub case_id: String,
    pub advertised: bool,
    pub status: BridgeContractStatus,
    pub duration_ms: u64,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeContractSummary {
    pub evidence_kind: BridgeContractEvidenceKind,
    pub gate_passed: bool,
    pub failed_cases: Vec<String>,
}

impl BridgeContractSummary {
    pub fn evaluate(
        cases: &[BridgeContractCase],
        results: &[BridgeContractCaseResult],
        evidence_kind: BridgeContractEvidenceKind,
    ) -> VibexResult<Self> {
        let by_id: BTreeMap<_, _> = results
            .iter()
            .map(|result| (result.case_id.as_str(), result))
            .collect();
        if by_id.len() != results.len() {
            return Err(VibexError::validation(
                "acp_bridge_contract_duplicate_result",
                "ACP bridge contract contains duplicate case results",
            ));
        }
        let expected_ids: BTreeSet<_> = cases.iter().map(|case| case.id.as_str()).collect();
        let actual_ids: BTreeSet<_> = by_id.keys().copied().collect();
        if actual_ids != expected_ids {
            return Err(VibexError::validation(
                "acp_bridge_contract_result_set_mismatch",
                "ACP bridge contract result set does not match the contract matrix",
            ));
        }

        let mut failed_cases = Vec::new();
        for case in cases {
            let result = by_id.get(case.id.as_str()).ok_or_else(|| {
                VibexError::validation(
                    "acp_bridge_contract_result_missing",
                    "ACP bridge contract is missing a case result",
                )
                .with_diagnostic("caseId", case.id.clone())
            })?;
            let passed = match case.requirement {
                BridgeContractRequirement::Required => {
                    result.status == BridgeContractStatus::Passed
                }
                BridgeContractRequirement::WhenAdvertised => {
                    if result.advertised {
                        result.status == BridgeContractStatus::Passed
                    } else {
                        result.status == BridgeContractStatus::NotAdvertised
                    }
                }
                BridgeContractRequirement::NotApplicable => {
                    result.status == BridgeContractStatus::NotAdvertised
                }
            };
            if !passed {
                failed_cases.push(case.id.clone());
            }
        }

        // Fixture evidence validates the runner, never a production baseline.
        let gate_passed = failed_cases.is_empty()
            && evidence_kind == BridgeContractEvidenceKind::RealManagedAdapter;
        Ok(Self {
            evidence_kind,
            gate_passed,
            failed_cases,
        })
    }
}

/// Immutable exact-version descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAgentCompatibility {
    pub agent_id: AgentId,
    pub adapter_id: AcpAdapterId,
    pub distribution: AcpAdapterDistribution,
    pub command_variants: Vec<CommandVariant>,
    pub mcp_forwarding: CompatibilitySupport,
    pub safe_multi_session: CompatibilitySupport,
    pub restore_policy: RestorePolicy,
    pub operation_support: BTreeMap<AcpOperation, VersionedOperationDescriptor>,
    pub native_state_home_policy: NativeStateHomePolicy,
    pub config_option_aliases: BTreeMap<String, Vec<String>>,
    pub transcript_strategy: TranscriptStrategy,
    pub event_enricher: AgentEventEnricherKind,
    pub known_quirks: Vec<VersionedAgentQuirk>,
    pub bridge_contract: Vec<BridgeContractCase>,
}

impl AcpAgentCompatibility {
    pub fn route_key(&self) -> AgentRuntimeRouteKey {
        AgentRuntimeRouteKey {
            agent_id: self.agent_id.clone(),
            transport_kind: TransportKind::Acp,
            adapter_id: self.adapter_id.clone(),
        }
    }

    pub fn expected_compatibility_identity(&self) -> AdapterCompatibilityIdentity {
        let runtime_versions = self
            .distribution
            .runtime_dependencies
            .iter()
            .filter(|dependency| dependency.include_in_compatibility_identity)
            .map(|dependency| (dependency.package.clone(), dependency.managed_pin.clone()))
            .collect();
        AdapterCompatibilityIdentity::new(
            &self.adapter_id,
            &self.distribution.exact_version,
            &runtime_versions,
        )
    }

    pub fn quirks_for_identity(
        &self,
        identity: &AdapterCompatibilityIdentity,
    ) -> Vec<&VersionedAgentQuirk> {
        self.known_quirks
            .iter()
            .filter(|quirk| &quirk.compatibility_identity == identity)
            .collect()
    }

    pub fn event_enricher_for_identity(
        &self,
        identity: &AdapterCompatibilityIdentity,
    ) -> Option<AgentEventEnricherKind> {
        (&self.expected_compatibility_identity() == identity).then_some(self.event_enricher)
    }

    pub fn config_aliases_for_identity<'a>(
        &'a self,
        identity: &AdapterCompatibilityIdentity,
        canonical_key: &str,
    ) -> Option<&'a [String]> {
        if &self.expected_compatibility_identity() != identity {
            return None;
        }
        self.config_option_aliases
            .get(canonical_key)
            .map(Vec::as_slice)
    }

    fn validate(&self) -> VibexResult<()> {
        if !is_safe_managed_path_segment(self.adapter_id.as_str()) {
            return Err(VibexError::validation(
                "acp_registry_adapter_id_path_invalid",
                "Managed ACP adapter id must be one safe path segment",
            )
            .with_diagnostic("adapterId", self.adapter_id.to_string()));
        }
        if !is_safe_npm_package_name(&self.distribution.package) {
            return Err(VibexError::validation(
                "acp_registry_package_name_invalid",
                "ACP registry package name is not safe for a managed install path",
            )
            .with_diagnostic("adapterId", self.adapter_id.to_string()));
        }
        if !is_safe_registry_origin(&self.distribution.registry_origin) {
            return Err(VibexError::validation(
                "acp_registry_origin_invalid",
                "ACP managed package registry must be a credential-free HTTPS origin",
            )
            .with_diagnostic("adapterId", self.adapter_id.to_string()));
        }
        if self.command_variants.is_empty() {
            return Err(VibexError::validation(
                "acp_registry_command_missing",
                "ACP compatibility descriptor must declare a command",
            )
            .with_diagnostic("adapterId", self.adapter_id.to_string()));
        }
        if self
            .command_variants
            .iter()
            .any(|command| command.bin_name != self.distribution.bin_name)
        {
            return Err(VibexError::validation(
                "acp_registry_command_bin_mismatch",
                "ACP descriptor command must use the distribution bin",
            )
            .with_diagnostic("adapterId", self.adapter_id.to_string()));
        }
        if self.distribution.integrity.trim().is_empty()
            || !self.distribution.integrity.starts_with("sha512-")
        {
            return Err(VibexError::validation(
                "acp_registry_integrity_invalid",
                "ACP managed package must pin a sha512 npm integrity",
            )
            .with_diagnostic("adapterId", self.adapter_id.to_string()));
        }
        if self.distribution.initialize_agent_name.trim().is_empty() {
            return Err(VibexError::validation(
                "acp_registry_initialize_agent_name_missing",
                "ACP descriptor must declare the initialize agent name",
            )
            .with_diagnostic("adapterId", self.adapter_id.to_string()));
        }
        let mut dependency_packages = BTreeSet::new();
        for dependency in &self.distribution.runtime_dependencies {
            if !is_safe_npm_package_name(&dependency.package)
                || !dependency.integrity.starts_with("sha512-")
            {
                return Err(VibexError::validation(
                    "acp_registry_runtime_dependency_invalid",
                    "ACP managed runtime dependency is not safely pinned",
                )
                .with_diagnostic("adapterId", self.adapter_id.to_string())
                .with_diagnostic("package", dependency.package.clone()));
            }
            if !dependency_packages.insert(dependency.package.as_str()) {
                return Err(VibexError::validation(
                    "acp_registry_runtime_dependency_duplicate",
                    "ACP descriptor contains a duplicate managed runtime dependency",
                )
                .with_diagnostic("adapterId", self.adapter_id.to_string())
                .with_diagnostic("package", dependency.package.clone()));
            }
            if !dependency
                .declared_requirement
                .matches(&dependency.managed_pin)
            {
                return Err(VibexError::validation(
                    "acp_registry_runtime_dependency_pin_invalid",
                    "ACP managed runtime pin does not satisfy the adapter requirement",
                )
                .with_diagnostic("adapterId", self.adapter_id.to_string())
                .with_diagnostic("package", dependency.package.clone()));
            }
        }
        if self.distribution.node_requirement_package != self.distribution.package
            && !dependency_packages.contains(self.distribution.node_requirement_package.as_str())
        {
            return Err(VibexError::validation(
                "acp_registry_node_requirement_source_invalid",
                "ACP Node requirement source must be the adapter or a managed dependency",
            )
            .with_diagnostic("adapterId", self.adapter_id.to_string()));
        }
        let operation_keys: BTreeSet<_> = self.operation_support.keys().cloned().collect();
        if operation_keys.len() != self.operation_support.len() {
            return Err(VibexError::validation(
                "acp_registry_operation_duplicate",
                "ACP descriptor contains duplicate operation entries",
            ));
        }
        for (operation, descriptor) in &self.operation_support {
            if operation != &descriptor.operation {
                return Err(VibexError::validation(
                    "acp_registry_operation_key_mismatch",
                    "ACP descriptor operation key does not match its value",
                ));
            }
        }
        validate_contract_cases(&self.bridge_contract)?;
        let expected_identity = self.expected_compatibility_identity();
        if self
            .known_quirks
            .iter()
            .any(|quirk| quirk.compatibility_identity != expected_identity)
        {
            return Err(VibexError::validation(
                "acp_registry_quirk_identity_mismatch",
                "ACP quirk must target the descriptor's exact compatibility identity",
            )
            .with_diagnostic("adapterId", self.adapter_id.to_string()));
        }
        Ok(())
    }
}

pub(crate) fn is_safe_managed_path_segment(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\', ':', '\0'])
}

fn is_safe_npm_package_name(package: &str) -> bool {
    if package.is_empty() || package.contains(['\\', ':', '\0']) {
        return false;
    }
    let segments: Vec<_> = package.split('/').collect();
    let valid_shape = match segments.as_slice() {
        [name] => !name.starts_with('@'),
        [scope, name] => scope.starts_with('@') && scope.len() > 1 && !name.starts_with('@'),
        _ => false,
    };
    valid_shape
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && *segment != "." && *segment != "..")
}

fn is_safe_registry_origin(value: &str) -> bool {
    let Some(authority) = value.strip_prefix("https://") else {
        return false;
    };
    !authority.is_empty()
        && !authority.contains(['/', '?', '#', '@', '\\', '\0'])
        && authority
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-'))
}

/// Registry indexed independently by agent and adapter identity.
#[derive(Debug, Clone, Default)]
pub struct AcpCompatibilityRegistry {
    by_agent: BTreeMap<AgentId, AcpAgentCompatibility>,
    agent_by_adapter: BTreeMap<AcpAdapterId, AgentId>,
}

impl AcpCompatibilityRegistry {
    pub fn builtin() -> VibexResult<Self> {
        let mut registry = Self::default();
        registry.register(claude_descriptor()?)?;
        registry.register(codex_descriptor()?)?;
        Ok(registry)
    }

    pub fn register(&mut self, descriptor: AcpAgentCompatibility) -> VibexResult<()> {
        descriptor.validate()?;
        if self.by_agent.contains_key(&descriptor.agent_id) {
            return Err(VibexError::conflict(
                "acp_registry_agent_duplicate",
                "ACP compatibility descriptor is already registered for this agent",
            )
            .with_diagnostic("agentId", descriptor.agent_id.to_string()));
        }
        if self.agent_by_adapter.contains_key(&descriptor.adapter_id) {
            return Err(VibexError::conflict(
                "acp_registry_adapter_duplicate",
                "ACP compatibility descriptor is already registered for this adapter",
            )
            .with_diagnostic("adapterId", descriptor.adapter_id.to_string()));
        }
        self.agent_by_adapter
            .insert(descriptor.adapter_id.clone(), descriptor.agent_id.clone());
        self.by_agent
            .insert(descriptor.agent_id.clone(), descriptor);
        Ok(())
    }

    pub fn for_agent(&self, agent_id: &AgentId) -> Option<&AcpAgentCompatibility> {
        self.by_agent.get(agent_id)
    }

    pub fn for_adapter(&self, adapter_id: &AcpAdapterId) -> Option<&AcpAgentCompatibility> {
        let agent_id = self.agent_by_adapter.get(adapter_id)?;
        self.by_agent.get(agent_id)
    }

    pub fn route_key(&self, agent_id: &AgentId) -> VibexResult<AgentRuntimeRouteKey> {
        self.for_agent(agent_id)
            .map(AcpAgentCompatibility::route_key)
            .ok_or_else(|| {
                VibexError::capability(
                    "acp_registry_agent_unknown",
                    "No managed ACP compatibility descriptor exists for this agent",
                )
                .with_diagnostic("agentId", agent_id.to_string())
            })
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &AcpAgentCompatibility> {
        self.by_agent.values()
    }
}

/// Runtime evidence scoped to one exact adapter process generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEvidence {
    pub support: CapabilitySupport,
    pub source: CapabilitySource,
    pub compatibility_identity: AdapterCompatibilityIdentity,
    pub activation_generation: i64,
    pub detail: String,
}

pub struct CapabilityResolutionInput<'a> {
    pub compatibility_identity: &'a AdapterCompatibilityIdentity,
    pub activation_generation: i64,
    pub negotiated: Option<&'a CapabilityEvidence>,
    pub observed: Option<&'a CapabilityEvidence>,
    pub registry: Option<CapabilitySupport>,
    pub profile: Option<CapabilitySupport>,
    pub conservative: CapabilitySupport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCapability {
    pub support: CapabilitySupport,
    pub source: CapabilitySource,
}

impl ResolvedCapability {
    pub fn resolve(input: CapabilityResolutionInput<'_>) -> Self {
        let negotiated = scoped_runtime_evidence(
            input.negotiated,
            input.compatibility_identity,
            input.activation_generation,
        );
        let observed = scoped_runtime_evidence(
            input.observed,
            input.compatibility_identity,
            input.activation_generation,
        );

        // A live method-not-found or equivalent negative is newer evidence
        // than initialize and must downgrade exactly this operation.
        if observed.is_some_and(|evidence| evidence.support == CapabilitySupport::Unsupported) {
            return Self {
                support: CapabilitySupport::Unsupported,
                source: CapabilitySource::ObservedRuntime,
            };
        }
        if let Some(evidence) = negotiated
            && evidence.support != CapabilitySupport::Unknown
        {
            return Self {
                support: evidence.support,
                source: CapabilitySource::NegotiatedRuntime,
            };
        }
        if let Some(evidence) = observed
            && evidence.support != CapabilitySupport::Unknown
        {
            return Self {
                support: evidence.support,
                source: CapabilitySource::ObservedRuntime,
            };
        }
        if let Some(support) = input.registry
            && support != CapabilitySupport::Unknown
        {
            return Self {
                support,
                source: CapabilitySource::VersionedRegistry,
            };
        }
        if let Some(support) = input.profile
            && support != CapabilitySupport::Unknown
        {
            return Self {
                support,
                source: CapabilitySource::DeclaredProfile,
            };
        }
        Self {
            support: input.conservative,
            source: CapabilitySource::ConservativeDefault,
        }
    }
}

fn scoped_runtime_evidence<'a>(
    evidence: Option<&'a CapabilityEvidence>,
    compatibility_identity: &AdapterCompatibilityIdentity,
    activation_generation: i64,
) -> Option<&'a CapabilityEvidence> {
    evidence.filter(|evidence| {
        &evidence.compatibility_identity == compatibility_identity
            && evidence.activation_generation == activation_generation
    })
}

fn base_operation_support() -> BTreeMap<AcpOperation, VersionedOperationDescriptor> {
    baseline_operation_matrix()
        .into_iter()
        .map(|entry| {
            let operation = entry.operation;
            let support = match operation {
                AcpOperation::Initialize
                | AcpOperation::SessionNew
                | AcpOperation::SessionPrompt
                | AcpOperation::SessionCancel
                | AcpOperation::SessionUpdate => CapabilitySupport::Supported,
                _ => CapabilitySupport::Unknown,
            };
            (
                operation.clone(),
                VersionedOperationDescriptor {
                    operation,
                    support,
                    stability: entry.stability,
                    encoding: entry.encoding,
                },
            )
        })
        .collect()
}

fn bridge_contract_cases() -> Vec<BridgeContractCase> {
    use BridgeContractRequirement::{Required, WhenAdvertised};
    vec![
        contract_operation("initialize", AcpOperation::Initialize, Required),
        contract_operation("session_new", AcpOperation::SessionNew, Required),
        contract_operation("session_resume", AcpOperation::SessionResume, Required),
        contract_operation("session_load", AcpOperation::SessionLoad, Required),
        contract_operation("session_prompt", AcpOperation::SessionPrompt, Required),
        contract_operation("session_cancel", AcpOperation::SessionCancel, Required),
        contract_operation("session_set_mode", AcpOperation::SessionSetMode, Required),
        contract_operation(
            "session_set_config_option",
            AcpOperation::SessionSetConfigOption,
            Required,
        ),
        contract_operation(
            "session_set_model",
            AcpOperation::SessionSetModel,
            WhenAdvertised,
        ),
        contract_operation("session_fork", AcpOperation::SessionFork, WhenAdvertised),
        contract_feature("permission", Required),
        contract_feature("mcp", Required),
        contract_feature("attachments", Required),
    ]
}

fn contract_operation(
    id: &str,
    operation: AcpOperation,
    requirement: BridgeContractRequirement,
) -> BridgeContractCase {
    BridgeContractCase {
        id: id.to_string(),
        operation: Some(operation),
        requirement,
    }
}

fn contract_feature(id: &str, requirement: BridgeContractRequirement) -> BridgeContractCase {
    BridgeContractCase {
        id: id.to_string(),
        operation: None,
        requirement,
    }
}

fn validate_contract_cases(cases: &[BridgeContractCase]) -> VibexResult<()> {
    let expected: BTreeSet<_> = [
        "initialize",
        "session_new",
        "session_resume",
        "session_load",
        "session_prompt",
        "session_cancel",
        "session_set_mode",
        "session_set_config_option",
        "session_set_model",
        "session_fork",
        "permission",
        "mcp",
        "attachments",
    ]
    .into_iter()
    .collect();
    let actual: BTreeSet<_> = cases.iter().map(|case| case.id.as_str()).collect();
    if actual.len() != cases.len() {
        return Err(VibexError::validation(
            "acp_bridge_contract_case_duplicate",
            "ACP bridge contract contains duplicate cases",
        ));
    }
    if actual != expected {
        return Err(VibexError::validation(
            "acp_bridge_contract_case_incomplete",
            "ACP bridge contract does not cover the complete section 25.2 matrix",
        ));
    }
    Ok(())
}

fn claude_descriptor() -> VibexResult<AcpAgentCompatibility> {
    let adapter_id = AcpAdapterId::parse(CLAUDE_ADAPTER_ID)?;
    let exact_version = parse_version(CLAUDE_ADAPTER_VERSION, CLAUDE_ADAPTER_ID)?;
    let identity = AdapterCompatibilityIdentity::new(&adapter_id, &exact_version, &BTreeMap::new());
    Ok(AcpAgentCompatibility {
        agent_id: AgentId::parse(CLAUDE_AGENT_ID)?,
        adapter_id,
        distribution: AcpAdapterDistribution {
            registry_origin: NPM_REGISTRY_ORIGIN.to_string(),
            package: CLAUDE_ADAPTER_PACKAGE.to_string(),
            exact_version,
            integrity: CLAUDE_ADAPTER_INTEGRITY.to_string(),
            bin_name: CLAUDE_ADAPTER_ID.to_string(),
            initialize_agent_name: CLAUDE_ADAPTER_PACKAGE.to_string(),
            node_requirement: parse_requirement(">=22", CLAUDE_ADAPTER_ID)?,
            node_requirement_package: CLAUDE_ADAPTER_PACKAGE.to_string(),
            runtime_dependencies: Vec::new(),
        },
        command_variants: vec![CommandVariant {
            bin_name: CLAUDE_ADAPTER_ID.to_string(),
            args: Vec::new(),
            version_args: vec!["--version".to_string()],
        }],
        mcp_forwarding: CompatibilitySupport::supported(
            "real bridge contract schema v2: claude-agent-acp@0.58.1",
        ),
        safe_multi_session: CompatibilitySupport::unsupported(
            "no exact-version multi-session contract evidence",
        ),
        restore_policy: RestorePolicy::LoadThenNew,
        operation_support: base_operation_support(),
        native_state_home_policy: NativeStateHomePolicy::ClaudeConfigDirectory,
        config_option_aliases: BTreeMap::from([
            ("model".to_string(), vec!["model".to_string()]),
            (
                "reasoning_effort".to_string(),
                vec!["effort".to_string(), "thinking_level".to_string()],
            ),
        ]),
        transcript_strategy: TranscriptStrategy::ClaudeJsonl,
        event_enricher: AgentEventEnricherKind::Claude,
        known_quirks: vec![VersionedAgentQuirk {
            id: "claude-sdk-extension-notifications".to_string(),
            compatibility_identity: identity,
            operation: None,
            summary: "decode Claude SDK extension notifications only at this exact identity"
                .to_string(),
        }],
        bridge_contract: bridge_contract_cases(),
    })
}

fn codex_descriptor() -> VibexResult<AcpAgentCompatibility> {
    let adapter_id = AcpAdapterId::parse(CODEX_ADAPTER_ID)?;
    let exact_version = parse_version(CODEX_ADAPTER_VERSION, CODEX_ADAPTER_ID)?;
    let runtime_dependency = ManagedRuntimeDependency {
        package: CODEX_RUNTIME_PACKAGE.to_string(),
        declared_requirement: parse_requirement(
            CODEX_RUNTIME_DECLARED_REQUIREMENT,
            CODEX_ADAPTER_ID,
        )?,
        managed_pin: parse_version(CODEX_RUNTIME_PIN, CODEX_RUNTIME_PACKAGE)?,
        integrity: CODEX_RUNTIME_INTEGRITY.to_string(),
        include_in_compatibility_identity: true,
    };
    let runtime_versions = BTreeMap::from([(
        runtime_dependency.package.clone(),
        runtime_dependency.managed_pin.clone(),
    )]);
    let identity =
        AdapterCompatibilityIdentity::new(&adapter_id, &exact_version, &runtime_versions);
    Ok(AcpAgentCompatibility {
        agent_id: AgentId::parse(CODEX_AGENT_ID)?,
        adapter_id,
        distribution: AcpAdapterDistribution {
            registry_origin: NPM_REGISTRY_ORIGIN.to_string(),
            package: CODEX_ADAPTER_PACKAGE.to_string(),
            exact_version,
            integrity: CODEX_ADAPTER_INTEGRITY.to_string(),
            bin_name: CODEX_ADAPTER_ID.to_string(),
            initialize_agent_name: CODEX_ADAPTER_PACKAGE.to_string(),
            node_requirement: parse_requirement(">=16", CODEX_ADAPTER_ID)?,
            node_requirement_package: CODEX_RUNTIME_PACKAGE.to_string(),
            runtime_dependencies: vec![runtime_dependency],
        },
        command_variants: vec![CommandVariant {
            bin_name: CODEX_ADAPTER_ID.to_string(),
            args: Vec::new(),
            version_args: vec!["--version".to_string()],
        }],
        mcp_forwarding: CompatibilitySupport::supported(
            "real bridge contract schema v2: codex-acp@1.1.2 + @openai/codex@0.144.1",
        ),
        safe_multi_session: CompatibilitySupport::unsupported(
            "no exact-version multi-session contract evidence",
        ),
        restore_policy: RestorePolicy::ResumeThenLoadThenNew,
        operation_support: base_operation_support(),
        native_state_home_policy: NativeStateHomePolicy::StableCodexHome,
        config_option_aliases: BTreeMap::from([
            ("model".to_string(), vec!["model".to_string()]),
            (
                "reasoning_effort".to_string(),
                vec!["effort".to_string(), "reasoning_effort".to_string()],
            ),
            (
                "approval_mode".to_string(),
                vec!["approval_policy".to_string()],
            ),
            ("sandbox_mode".to_string(), vec!["sandbox".to_string()]),
        ]),
        transcript_strategy: TranscriptStrategy::CodexRollout,
        event_enricher: AgentEventEnricherKind::Codex,
        known_quirks: vec![VersionedAgentQuirk {
            id: "codex-legacy-set-model-extension".to_string(),
            compatibility_identity: identity,
            operation: Some(AcpOperation::SessionSetModel),
            summary: "session/set_model is an exact-version legacy extension, not generic ACP"
                .to_string(),
        }],
        bridge_contract: bridge_contract_cases(),
    })
}

/// Session modes and reasoning-effort levels pinned per adapter identity.
///
/// These tables mirror what the exact registry-managed adapter version
/// advertises at runtime; they exist so selectors and validation keep working
/// when a live probe is unavailable. Conditional values the adapter may
/// refuse (root-gated `bypassPermissions`, model-gated `auto`/`xhigh`) are
/// only listed in the `known_*` acceptance sets, never in the fallbacks.
mod pinned {
    pub const CLAUDE_FALLBACK_MODES: &[(&str, &str)] = &[
        ("default", "Manual"),
        ("acceptEdits", "Accept Edits"),
        ("plan", "Plan Mode"),
        ("dontAsk", "Don't Ask"),
    ];
    pub const CLAUDE_KNOWN_MODES: &[&str] = &[
        "default",
        "acceptEdits",
        "plan",
        "dontAsk",
        "auto",
        "bypassPermissions",
    ];
    pub const CLAUDE_FALLBACK_EFFORTS: &[(&str, &str)] = &[
        ("low", "Low"),
        ("medium", "Medium"),
        ("high", "High"),
        ("max", "Max"),
    ];
    pub const CLAUDE_KNOWN_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
}

/// Catalog fallback: session modes the pinned claude-agent-acp adapter
/// advertises in every `session/new` response regardless of model or
/// environment.
pub fn fallback_session_modes(agent_id: &AgentId) -> Vec<ProviderSessionConfigValue> {
    if agent_id.as_str() != CLAUDE_AGENT_ID {
        return Vec::new();
    }
    pinned::CLAUDE_FALLBACK_MODES
        .iter()
        .map(|(value, label)| ProviderSessionConfigValue {
            value: (*value).to_string(),
            label: Some((*label).to_string()),
        })
        .collect()
}

/// Validation acceptance set: every session mode the pinned adapter can
/// advertise, including environment- and model-gated ones.
pub fn known_session_mode_values(agent_id: &AgentId) -> &'static [&'static str] {
    if agent_id.as_str() == CLAUDE_AGENT_ID {
        pinned::CLAUDE_KNOWN_MODES
    } else {
        &[]
    }
}

/// Catalog fallback: reasoning-effort levels every effort-capable model of
/// the pinned adapter supports.
pub fn fallback_reasoning_efforts(agent_id: &AgentId) -> Vec<AgentReasoningEffort> {
    if agent_id.as_str() != CLAUDE_AGENT_ID {
        return Vec::new();
    }
    pinned::CLAUDE_FALLBACK_EFFORTS
        .iter()
        .map(|(value, label)| AgentReasoningEffort {
            value: (*value).to_string(),
            description: Some((*label).to_string()),
        })
        .collect()
}

/// Validation acceptance set: every reasoning-effort level the pinned
/// adapter can expose, including model-gated ones.
pub fn known_reasoning_effort_values(agent_id: &AgentId) -> &'static [&'static str] {
    if agent_id.as_str() == CLAUDE_AGENT_ID {
        pinned::CLAUDE_KNOWN_EFFORTS
    } else {
        &[]
    }
}

/// Every ACP Agent can be asked for session configuration through the
/// protocol's required `session/new` handshake. Callers treat probe failure
/// as empty evidence so third-party Agents remain fail-soft.
pub fn agent_supports_session_config_probe(_agent_id: &AgentId) -> bool {
    true
}

fn parse_version(value: &str, owner: &str) -> VibexResult<Version> {
    Version::parse(value).map_err(|error| {
        VibexError::validation(
            "acp_registry_version_invalid",
            "ACP registry contains an invalid semantic version",
        )
        .with_diagnostic("owner", owner)
        .with_diagnostic("error", error.to_string())
    })
}

fn parse_requirement(value: &str, owner: &str) -> VibexResult<VersionReq> {
    VersionReq::parse(value).map_err(|error| {
        VibexError::validation(
            "acp_registry_version_requirement_invalid",
            "ACP registry contains an invalid semantic version requirement",
        )
        .with_diagnostic("owner", owner)
        .with_diagnostic("error", error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> AcpCompatibilityRegistry {
        AcpCompatibilityRegistry::builtin().unwrap()
    }

    #[test]
    fn pinned_session_config_knowledge_is_scoped_and_catalog_safe() {
        let claude = AgentId::parse(CLAUDE_AGENT_ID).unwrap();
        let codex = AgentId::parse(CODEX_AGENT_ID).unwrap();

        let fallback_modes = fallback_session_modes(&claude);
        assert_eq!(
            fallback_modes
                .iter()
                .map(|mode| mode.value.as_str())
                .collect::<Vec<_>>(),
            vec!["default", "acceptEdits", "plan", "dontAsk"]
        );
        let fallback_efforts = fallback_reasoning_efforts(&claude);
        assert_eq!(
            fallback_efforts
                .iter()
                .map(|effort| effort.value.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "medium", "high", "max"]
        );

        // Every pinned value must survive catalog validation, and fallbacks
        // must stay inside the validation acceptance sets.
        for mode in &fallback_modes {
            assert!(crate::session_config::validate_effort_value(&mode.value).is_ok());
            assert!(known_session_mode_values(&claude).contains(&mode.value.as_str()));
        }
        for effort in &fallback_efforts {
            assert!(crate::session_config::validate_effort_value(&effort.value).is_ok());
            assert!(known_reasoning_effort_values(&claude).contains(&effort.value.as_str()));
        }

        assert!(fallback_session_modes(&codex).is_empty());
        assert!(fallback_reasoning_efforts(&codex).is_empty());
        assert!(known_session_mode_values(&codex).is_empty());
        assert!(known_reasoning_effort_values(&codex).is_empty());

        assert!(agent_supports_session_config_probe(&claude));
        assert!(agent_supports_session_config_probe(&codex));
        assert!(agent_supports_session_config_probe(
            &AgentId::parse("gemini").unwrap()
        ));
    }

    #[test]
    fn builtin_descriptors_pin_exact_packages_commands_and_routes() {
        let registry = registry();
        let claude = registry
            .for_agent(&AgentId::parse(CLAUDE_AGENT_ID).unwrap())
            .unwrap();
        assert_eq!(claude.adapter_id.as_str(), CLAUDE_ADAPTER_ID);
        assert_eq!(
            claude.distribution.package_spec(),
            format!("{CLAUDE_ADAPTER_PACKAGE}@{CLAUDE_ADAPTER_VERSION}")
        );
        assert_eq!(claude.command_variants[0].bin_name, CLAUDE_ADAPTER_ID);
        assert_eq!(claude.distribution.registry_origin, NPM_REGISTRY_ORIGIN);

        let codex = registry
            .for_adapter(&AcpAdapterId::parse(CODEX_ADAPTER_ID).unwrap())
            .unwrap();
        assert_eq!(codex.agent_id.as_str(), CODEX_AGENT_ID);
        assert_eq!(
            codex.distribution.package_spec(),
            format!("{CODEX_ADAPTER_PACKAGE}@{CODEX_ADAPTER_VERSION}")
        );
        assert_eq!(codex.route_key().adapter_id.as_str(), CODEX_ADAPTER_ID);
        assert_eq!(
            registry
                .route_key(&AgentId::parse(CLAUDE_AGENT_ID).unwrap())
                .unwrap()
                .adapter_id
                .as_str(),
            CLAUDE_ADAPTER_ID
        );
    }

    #[test]
    fn descriptor_rejects_non_https_or_credentialed_registry_origins() {
        let mut descriptor = registry()
            .for_agent(&AgentId::parse(CLAUDE_AGENT_ID).unwrap())
            .unwrap()
            .clone();
        for origin in [
            "http://registry.npmjs.org",
            "https://user@registry.npmjs.org",
            "https://registry.npmjs.org/path",
            "file:///tmp/registry",
        ] {
            descriptor.distribution.registry_origin = origin.to_string();
            let error = descriptor.validate().unwrap_err();
            assert_eq!(error.code, "acp_registry_origin_invalid");
        }
    }

    #[test]
    fn unknown_agents_do_not_fall_back_to_a_builtin_descriptor() {
        let err = registry()
            .route_key(&AgentId::parse("opencode").unwrap())
            .unwrap_err();
        assert_eq!(err.code, "acp_registry_agent_unknown");
    }

    #[test]
    fn duplicate_agent_and_adapter_registrations_are_rejected() {
        let mut registry = registry();
        let claude = registry
            .for_agent(&AgentId::parse(CLAUDE_AGENT_ID).unwrap())
            .unwrap()
            .clone();
        let err = registry.register(claude.clone()).unwrap_err();
        assert_eq!(err.code, "acp_registry_agent_duplicate");

        let mut duplicate_adapter = claude;
        duplicate_adapter.agent_id = AgentId::parse("other-claude").unwrap();
        let err = registry.register(duplicate_adapter).unwrap_err();
        assert_eq!(err.code, "acp_registry_adapter_duplicate");
    }

    #[test]
    fn codex_runtime_version_is_part_of_canonical_identity() {
        let codex = registry()
            .for_agent(&AgentId::parse(CODEX_AGENT_ID).unwrap())
            .unwrap()
            .clone();
        assert_eq!(
            codex.expected_compatibility_identity().as_str(),
            "adapter=codex-acp@1.1.2;runtime=@openai/codex@0.144.1"
        );

        let changed = AdapterCompatibilityIdentity::new(
            &codex.adapter_id,
            &codex.distribution.exact_version,
            &BTreeMap::from([(
                CODEX_RUNTIME_PACKAGE.to_string(),
                Version::parse("0.144.0").unwrap(),
            )]),
        );
        assert_ne!(codex.expected_compatibility_identity(), changed);
    }

    #[test]
    fn quirks_aliases_and_enrichers_require_exact_identity() {
        let registry = registry();
        let codex = registry
            .for_agent(&AgentId::parse(CODEX_AGENT_ID).unwrap())
            .unwrap();
        let exact = codex.expected_compatibility_identity();
        assert_eq!(codex.quirks_for_identity(&exact).len(), 1);
        assert_eq!(
            codex.event_enricher_for_identity(&exact),
            Some(AgentEventEnricherKind::Codex)
        );
        assert!(
            codex
                .config_aliases_for_identity(&exact, "reasoning_effort")
                .is_some()
        );

        let adjacent = AdapterCompatibilityIdentity::new(
            &codex.adapter_id,
            &Version::parse("1.1.1").unwrap(),
            &BTreeMap::from([(
                CODEX_RUNTIME_PACKAGE.to_string(),
                Version::parse(CODEX_RUNTIME_PIN).unwrap(),
            )]),
        );
        assert!(codex.quirks_for_identity(&adjacent).is_empty());
        assert_eq!(codex.event_enricher_for_identity(&adjacent), None);
        assert_eq!(
            codex.config_aliases_for_identity(&adjacent, "reasoning_effort"),
            None
        );
    }

    fn evidence(
        support: CapabilitySupport,
        source: CapabilitySource,
        identity: &AdapterCompatibilityIdentity,
        generation: i64,
    ) -> CapabilityEvidence {
        CapabilityEvidence {
            support,
            source,
            compatibility_identity: identity.clone(),
            activation_generation: generation,
            detail: "test".to_string(),
        }
    }

    #[test]
    fn capability_priority_and_observed_negative_are_operation_local() {
        let identity = registry()
            .for_agent(&AgentId::parse(CODEX_AGENT_ID).unwrap())
            .unwrap()
            .expected_compatibility_identity();
        let negotiated = evidence(
            CapabilitySupport::Supported,
            CapabilitySource::NegotiatedRuntime,
            &identity,
            7,
        );
        let observed_negative = evidence(
            CapabilitySupport::Unsupported,
            CapabilitySource::ObservedRuntime,
            &identity,
            7,
        );
        let resolved = ResolvedCapability::resolve(CapabilityResolutionInput {
            compatibility_identity: &identity,
            activation_generation: 7,
            negotiated: Some(&negotiated),
            observed: Some(&observed_negative),
            registry: Some(CapabilitySupport::Supported),
            profile: Some(CapabilitySupport::Supported),
            conservative: CapabilitySupport::Unsupported,
        });
        assert_eq!(resolved.support, CapabilitySupport::Unsupported);
        assert_eq!(resolved.source, CapabilitySource::ObservedRuntime);

        let other_operation = ResolvedCapability::resolve(CapabilityResolutionInput {
            compatibility_identity: &identity,
            activation_generation: 7,
            negotiated: Some(&negotiated),
            observed: None,
            registry: Some(CapabilitySupport::Unsupported),
            profile: None,
            conservative: CapabilitySupport::Unsupported,
        });
        assert_eq!(other_operation.support, CapabilitySupport::Supported);
        assert_eq!(other_operation.source, CapabilitySource::NegotiatedRuntime);
    }

    #[test]
    fn capability_evidence_is_scoped_by_identity_and_generation() {
        let identity = registry()
            .for_agent(&AgentId::parse(CODEX_AGENT_ID).unwrap())
            .unwrap()
            .expected_compatibility_identity();
        let stale = evidence(
            CapabilitySupport::Supported,
            CapabilitySource::NegotiatedRuntime,
            &identity,
            6,
        );
        let resolved = ResolvedCapability::resolve(CapabilityResolutionInput {
            compatibility_identity: &identity,
            activation_generation: 7,
            negotiated: Some(&stale),
            observed: None,
            registry: Some(CapabilitySupport::Unknown),
            profile: Some(CapabilitySupport::Supported),
            conservative: CapabilitySupport::Unsupported,
        });
        assert_eq!(resolved.source, CapabilitySource::DeclaredProfile);

        let conservative = ResolvedCapability::resolve(CapabilityResolutionInput {
            compatibility_identity: &identity,
            activation_generation: 7,
            negotiated: None,
            observed: None,
            registry: Some(CapabilitySupport::Unknown),
            profile: Some(CapabilitySupport::Unknown),
            conservative: CapabilitySupport::Unsupported,
        });
        assert_eq!(conservative.source, CapabilitySource::ConservativeDefault);
    }

    #[test]
    fn bridge_contract_matrix_is_complete_and_fixture_never_validates_baseline() {
        let cases = bridge_contract_cases();
        validate_contract_cases(&cases).unwrap();
        let results: Vec<_> = cases
            .iter()
            .map(|case| BridgeContractCaseResult {
                case_id: case.id.clone(),
                advertised: case.requirement != BridgeContractRequirement::WhenAdvertised,
                status: if case.requirement == BridgeContractRequirement::WhenAdvertised {
                    BridgeContractStatus::NotAdvertised
                } else {
                    BridgeContractStatus::Passed
                },
                duration_ms: 1,
                error_code: None,
            })
            .collect();
        let fixture =
            BridgeContractSummary::evaluate(&cases, &results, BridgeContractEvidenceKind::Fixture)
                .unwrap();
        assert!(fixture.failed_cases.is_empty());
        assert!(!fixture.gate_passed);

        let real = BridgeContractSummary::evaluate(
            &cases,
            &results,
            BridgeContractEvidenceKind::RealManagedAdapter,
        )
        .unwrap();
        assert!(real.gate_passed);
    }

    #[test]
    fn blocked_or_advertised_failure_fails_contract_gate() {
        let cases = bridge_contract_cases();
        let mut results: Vec<_> = cases
            .iter()
            .map(|case| BridgeContractCaseResult {
                case_id: case.id.clone(),
                advertised: false,
                status: if case.requirement == BridgeContractRequirement::Required {
                    BridgeContractStatus::Passed
                } else {
                    BridgeContractStatus::NotAdvertised
                },
                duration_ms: 1,
                error_code: None,
            })
            .collect();
        let prompt = results
            .iter_mut()
            .find(|result| result.case_id == "session_prompt")
            .unwrap();
        prompt.status = BridgeContractStatus::Blocked;
        prompt.error_code = Some("provider_auth_required".to_string());
        let summary = BridgeContractSummary::evaluate(
            &cases,
            &results,
            BridgeContractEvidenceKind::RealManagedAdapter,
        )
        .unwrap();
        assert!(!summary.gate_passed);
        assert_eq!(summary.failed_cases, vec!["session_prompt"]);

        let fork = results
            .iter_mut()
            .find(|result| result.case_id == "session_fork")
            .unwrap();
        fork.advertised = true;
        fork.status = BridgeContractStatus::Failed;
        let summary = BridgeContractSummary::evaluate(
            &cases,
            &results,
            BridgeContractEvidenceKind::RealManagedAdapter,
        )
        .unwrap();
        assert!(summary.failed_cases.contains(&"session_fork".to_string()));
    }

    #[test]
    fn static_descriptors_only_claim_contract_verified_mcp() {
        for descriptor in registry().descriptors() {
            assert_ne!(
                descriptor.safe_multi_session.support,
                CapabilitySupport::Supported
            );
            assert_eq!(
                descriptor.mcp_forwarding.support,
                CapabilitySupport::Supported
            );
            assert!(descriptor.mcp_forwarding.evidence.contains("schema v2"));
        }
    }

    #[test]
    fn compatibility_support_supported_constructor_remains_available_for_custom_descriptors() {
        let support = CompatibilitySupport::supported("contract:test");
        assert_eq!(support.support, CapabilitySupport::Supported);
    }

    #[test]
    fn managed_descriptors_reject_path_escaping_adapter_and_package_names() {
        let mut descriptor = registry()
            .for_agent(&AgentId::parse(CLAUDE_AGENT_ID).unwrap())
            .unwrap()
            .clone();
        descriptor.agent_id = AgentId::parse("unsafe-adapter").unwrap();
        descriptor.adapter_id = AcpAdapterId::parse("../../outside").unwrap();
        let err = AcpCompatibilityRegistry::default()
            .register(descriptor.clone())
            .unwrap_err();
        assert_eq!(err.code, "acp_registry_adapter_id_path_invalid");

        descriptor.adapter_id = AcpAdapterId::parse("safe-adapter").unwrap();
        descriptor.distribution.package = "@scope/../outside".to_string();
        let err = AcpCompatibilityRegistry::default()
            .register(descriptor)
            .unwrap_err();
        assert_eq!(err.code, "acp_registry_package_name_invalid");
    }
}

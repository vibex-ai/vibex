use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vibex_core::{AcpAdapterId, VibexError, VibexResult};

use crate::{
    AcpAdapterHealthReport, AcpAgentCompatibility, AcpBridgeContractAdapterReport,
    AdapterCompatibilityIdentity, BridgeContractEvidenceKind, BridgeContractSummary,
    VerifiedAcpAdapterInstallation,
};

/// File written into one immutable managed-adapter installation directory.
pub const ACP_ACTIVATION_EVIDENCE_FILE: &str = "vibex-activation.json";
/// Current evidence schema. An unknown schema is rejected, never guessed.
pub const ACP_ACTIVATION_EVIDENCE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAcpAdapterActivation {
    adapter_id: AcpAdapterId,
    adapter_version: Version,
    compatibility_identity: AdapterCompatibilityIdentity,
    binary_identity: String,
}

impl VerifiedAcpAdapterActivation {
    pub fn verify(
        descriptor: &AcpAgentCompatibility,
        installation: &VerifiedAcpAdapterInstallation,
        health: &AcpAdapterHealthReport,
        contract: &AcpBridgeContractAdapterReport,
    ) -> VibexResult<Self> {
        let adapter_id = descriptor.adapter_id.as_str();
        let adapter_version = descriptor.distribution.exact_version.to_string();
        let compatibility_identity = descriptor.expected_compatibility_identity();
        let compatibility_identity_str = compatibility_identity.as_str();
        let binary_identity = installation.binary_identity.as_str();

        let installation_matches = installation.adapter_id == descriptor.adapter_id
            && installation.adapter_version == descriptor.distribution.exact_version
            && installation.compatibility_identity == compatibility_identity
            && !binary_identity.trim().is_empty();
        let health_matches = health.adapter_id == adapter_id
            && health.adapter_version == adapter_version
            && health.reported_adapter_version == adapter_version
            && health.agent_version.as_deref() == Some(adapter_version.as_str())
            && health.compatibility_identity == compatibility_identity_str
            && health.binary_identity == binary_identity;
        let contract_matches = contract.adapter_id == adapter_id
            && contract.adapter_version == adapter_version
            && contract.compatibility_identity == compatibility_identity_str
            && contract.binary_identity == binary_identity;

        if !installation_matches || !health_matches || !contract_matches {
            return Err(VibexError::validation(
                "acp_adapter_activation_identity_mismatch",
                "Managed ACP activation evidence does not describe one exact installation",
            )
            .with_diagnostic("adapterId", adapter_id));
        }
        if contract.summary.evidence_kind != BridgeContractEvidenceKind::RealManagedAdapter {
            return Err(VibexError::capability(
                "acp_adapter_activation_contract_gate_failed",
                "Managed ACP adapter did not pass the real Bridge Version Contract",
            )
            .with_diagnostic("adapterId", adapter_id));
        }
        let evaluated_summary = BridgeContractSummary::evaluate(
            &descriptor.bridge_contract,
            &contract.cases,
            BridgeContractEvidenceKind::RealManagedAdapter,
        )?;
        if evaluated_summary != contract.summary || !evaluated_summary.gate_passed {
            return Err(VibexError::capability(
                "acp_adapter_activation_contract_gate_failed",
                "Managed ACP adapter did not pass the real Bridge Version Contract",
            )
            .with_diagnostic("adapterId", adapter_id));
        }

        Ok(Self {
            adapter_id: descriptor.adapter_id.clone(),
            adapter_version: descriptor.distribution.exact_version.clone(),
            compatibility_identity,
            binary_identity: installation.binary_identity.clone(),
        })
    }

    pub fn adapter_id(&self) -> &AcpAdapterId {
        &self.adapter_id
    }

    pub fn adapter_version(&self) -> &Version {
        &self.adapter_version
    }

    pub fn compatibility_identity(&self) -> &AdapterCompatibilityIdentity {
        &self.compatibility_identity
    }

    pub fn binary_identity(&self) -> &str {
        &self.binary_identity
    }
}

/// Durable proof that one immutable managed-adapter installation was the
/// artifact described by its compatibility descriptor.
///
/// Written by the installer into the version directory, never by the runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAdapterActivationEvidence {
    pub schema_version: u32,
    pub adapter_id: String,
    pub adapter_version: String,
    pub compatibility_identity: String,
    /// Install-root-relative path of the file the adapter process executes.
    pub entrypoint: String,
    pub entrypoint_hash: String,
    pub runtime_versions: BTreeMap<String, String>,
    pub verified_at_ms: i64,
}

/// What the installation looks like right now, re-observed by the consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAdapterArtifactObservation {
    pub entrypoint_hash: String,
}

impl AcpAdapterArtifactObservation {
    /// Re-hashes the entrypoint named by `evidence` inside `install_root`.
    pub fn observe(
        install_root: &Path,
        evidence: &AcpAdapterActivationEvidence,
    ) -> VibexResult<Self> {
        let entrypoint = evidence_relative_path(&evidence.entrypoint)?;
        let root = install_root.canonicalize().map_err(|error| {
            activation_error(
                "acp_adapter_activation_evidence_invalid",
                "the managed installation root could not be resolved",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let path = root.join(entrypoint).canonicalize().map_err(|error| {
            activation_error(
                "acp_adapter_activation_evidence_invalid",
                "the recorded adapter entrypoint is not a file inside the installation",
            )
            .with_diagnostic("entrypoint", evidence.entrypoint.clone())
            .with_diagnostic("error", error.to_string())
        })?;
        if !path.starts_with(&root) || !path.is_file() {
            return Err(activation_error(
                "acp_adapter_activation_evidence_invalid",
                "the recorded adapter entrypoint is not a file inside the installation",
            )
            .with_diagnostic("entrypoint", evidence.entrypoint.clone()));
        }
        Ok(Self {
            entrypoint_hash: hash_activation_entrypoint(&path)?,
        })
    }
}

/// Reads the activation evidence for one installation directory.
pub fn read_activation_evidence(install_root: &Path) -> VibexResult<AcpAdapterActivationEvidence> {
    let path = install_root.join(ACP_ACTIVATION_EVIDENCE_FILE);
    let bytes = fs::read(&path).map_err(|error| {
        let code = if error.kind() == std::io::ErrorKind::NotFound {
            "acp_adapter_activation_evidence_missing"
        } else {
            "acp_adapter_activation_evidence_invalid"
        };
        activation_error(code, "managed ACP activation evidence could not be read")
            .with_diagnostic("error", error.to_string())
    })?;
    let evidence: AcpAdapterActivationEvidence =
        serde_json::from_slice(&bytes).map_err(|error| {
            activation_error(
                "acp_adapter_activation_evidence_invalid",
                "managed ACP activation evidence was not valid",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    if evidence.schema_version != ACP_ACTIVATION_EVIDENCE_SCHEMA_VERSION {
        return Err(activation_error(
            "acp_adapter_activation_evidence_invalid",
            "managed ACP activation evidence has an unsupported schema",
        )
        .with_diagnostic("schemaVersion", evidence.schema_version.to_string()));
    }
    Ok(evidence)
}

impl VerifiedAcpAdapterActivation {
    /// Production activation gate for a pinned managed adapter.
    ///
    /// Joins the descriptor's expected identity with the evidence recorded at
    /// install time and the artifact observed now. Bridge Version Contract
    /// cases are release-time evidence attached to the descriptor; they are not
    /// re-run here.
    pub fn verify_installed(
        descriptor: &AcpAgentCompatibility,
        evidence: &AcpAdapterActivationEvidence,
        observed: &AcpAdapterArtifactObservation,
    ) -> VibexResult<Self> {
        let adapter_id = AcpAdapterId::parse(evidence.adapter_id.clone())?;
        let adapter_version = Version::parse(&evidence.adapter_version).map_err(|error| {
            activation_error(
                "acp_adapter_activation_evidence_invalid",
                "managed ACP activation evidence carried an invalid adapter version",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let runtime_versions = evidence
            .runtime_versions
            .iter()
            .map(|(package, version)| {
                Version::parse(version)
                    .map(|version| (package.clone(), version))
                    .map_err(|error| {
                        activation_error(
                            "acp_adapter_activation_evidence_invalid",
                            "managed ACP activation evidence carried an invalid runtime version",
                        )
                        .with_diagnostic("package", package.clone())
                        .with_diagnostic("error", error.to_string())
                    })
            })
            .collect::<VibexResult<BTreeMap<_, _>>>()?;

        let expected = descriptor.expected_compatibility_identity();
        let recorded =
            AdapterCompatibilityIdentity::new(&adapter_id, &adapter_version, &runtime_versions);
        if recorded != expected
            || evidence.compatibility_identity != recorded.as_str()
            || adapter_version != descriptor.distribution.exact_version
        {
            return Err(activation_error(
                "acp_adapter_activation_identity_mismatch",
                "managed ACP activation evidence does not describe this descriptor",
            )
            .with_diagnostic("adapterId", descriptor.adapter_id.as_str().to_string()));
        }
        if observed.entrypoint_hash != evidence.entrypoint_hash {
            return Err(activation_error(
                "acp_adapter_activation_artifact_drift",
                "the installed adapter entrypoint changed after activation evidence was written",
            )
            .with_diagnostic("adapterId", descriptor.adapter_id.as_str().to_string()));
        }

        Ok(Self {
            adapter_id: descriptor.adapter_id.clone(),
            adapter_version: descriptor.distribution.exact_version.clone(),
            compatibility_identity: expected,
            binary_identity: observed.entrypoint_hash.clone(),
        })
    }
}

fn activation_error(code: &'static str, message: &'static str) -> VibexError {
    VibexError::validation(code, message)
}

/// Accepts only a normalized relative path that stays inside the install root.
fn evidence_relative_path(value: &str) -> VibexResult<PathBuf> {
    let path = Path::new(value);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(activation_error(
                    "acp_adapter_activation_evidence_invalid",
                    "the recorded adapter entrypoint escaped its installation",
                ));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(activation_error(
            "acp_adapter_activation_evidence_invalid",
            "the recorded adapter entrypoint was empty",
        ));
    }
    Ok(clean)
}

fn hash_activation_entrypoint(path: &Path) -> VibexResult<String> {
    let bytes = fs::read(path).map_err(|error| {
        activation_error(
            "acp_adapter_activation_evidence_invalid",
            "the installed adapter entrypoint could not be read",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let digest = Sha256::digest(&bytes);
    Ok(format!("sha256:{digest:x}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use sha2::{Digest, Sha256};

    use crate::{
        AcpCompatibilityRegistry, BridgeContractCaseResult, BridgeContractRequirement,
        BridgeContractStatus, ManagedAdapterCommand,
    };
    use vibex_core::AgentId;

    use super::*;

    fn evidence() -> (
        AcpAgentCompatibility,
        VerifiedAcpAdapterInstallation,
        AcpAdapterHealthReport,
        AcpBridgeContractAdapterReport,
    ) {
        let descriptor = AcpCompatibilityRegistry::builtin()
            .unwrap()
            .for_agent(&AgentId::parse("claude").unwrap())
            .unwrap()
            .clone();
        let identity = descriptor.expected_compatibility_identity();
        let installation = VerifiedAcpAdapterInstallation {
            adapter_id: descriptor.adapter_id.clone(),
            adapter_version: descriptor.distribution.exact_version.clone(),
            compatibility_identity: identity.clone(),
            binary_identity: "sha256:activation-test".to_string(),
            runtime_versions: BTreeMap::new(),
            install_root: PathBuf::from("/tmp/activation-test"),
            command: ManagedAdapterCommand {
                program: PathBuf::from("node"),
                args: Vec::new(),
                current_dir: PathBuf::from("/tmp/activation-test"),
            },
        };
        let version = descriptor.distribution.exact_version.to_string();
        let health = AcpAdapterHealthReport {
            adapter_id: descriptor.adapter_id.to_string(),
            adapter_version: version.clone(),
            compatibility_identity: identity.to_string(),
            binary_identity: installation.binary_identity.clone(),
            node_version: "22.0.0".to_string(),
            reported_adapter_version: version.clone(),
            protocol_version: Some("1".to_string()),
            agent_name: Some(descriptor.distribution.initialize_agent_name.clone()),
            agent_version: Some(version.clone()),
        };
        let cases: Vec<_> = descriptor
            .bridge_contract
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
        let summary = BridgeContractSummary::evaluate(
            &descriptor.bridge_contract,
            &cases,
            BridgeContractEvidenceKind::RealManagedAdapter,
        )
        .unwrap();
        let contract = AcpBridgeContractAdapterReport {
            adapter_id: descriptor.adapter_id.to_string(),
            adapter_version: version,
            compatibility_identity: identity.to_string(),
            binary_identity: installation.binary_identity.clone(),
            cases,
            summary,
        };
        (descriptor, installation, health, contract)
    }

    #[test]
    fn activation_requires_matching_install_health_and_real_contract() {
        let (descriptor, installation, health, contract) = evidence();
        let verified =
            VerifiedAcpAdapterActivation::verify(&descriptor, &installation, &health, &contract)
                .unwrap();
        assert_eq!(verified.binary_identity(), "sha256:activation-test");

        let mut wrong_health = health.clone();
        wrong_health.binary_identity = "sha256:other".to_string();
        assert_eq!(
            VerifiedAcpAdapterActivation::verify(
                &descriptor,
                &installation,
                &wrong_health,
                &contract,
            )
            .unwrap_err()
            .code,
            "acp_adapter_activation_identity_mismatch"
        );

        let mut failed_contract = contract;
        failed_contract.summary.gate_passed = false;
        assert_eq!(
            VerifiedAcpAdapterActivation::verify(
                &descriptor,
                &installation,
                &health,
                &failed_contract,
            )
            .unwrap_err()
            .code,
            "acp_adapter_activation_contract_gate_failed"
        );

        let (_, _, _, mut forged_contract) = evidence();
        let required = forged_contract
            .cases
            .iter_mut()
            .find(|case| case.case_id == "initialize")
            .unwrap();
        required.status = BridgeContractStatus::Failed;
        required.error_code = Some("forged-case-failure".to_string());
        assert!(forged_contract.summary.gate_passed);
        assert_eq!(
            VerifiedAcpAdapterActivation::verify(
                &descriptor,
                &installation,
                &health,
                &forged_contract,
            )
            .unwrap_err()
            .code,
            "acp_adapter_activation_contract_gate_failed"
        );
    }

    fn managed_evidence(
        descriptor: &AcpAgentCompatibility,
        root: &Path,
        entrypoint: &str,
        body: &[u8],
    ) -> AcpAdapterActivationEvidence {
        let path = root.join(entrypoint);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        let digest = Sha256::digest(body);
        AcpAdapterActivationEvidence {
            schema_version: ACP_ACTIVATION_EVIDENCE_SCHEMA_VERSION,
            adapter_id: descriptor.adapter_id.as_str().to_string(),
            adapter_version: descriptor.distribution.exact_version.to_string(),
            compatibility_identity: descriptor.expected_compatibility_identity().to_string(),
            entrypoint: entrypoint.to_string(),
            entrypoint_hash: format!("sha256:{digest:x}"),
            runtime_versions: descriptor
                .distribution
                .runtime_dependencies
                .iter()
                .filter(|dependency| dependency.include_in_compatibility_identity)
                .map(|dependency| {
                    (
                        dependency.package.clone(),
                        dependency.managed_pin.to_string(),
                    )
                })
                .collect(),
            verified_at_ms: 1,
        }
    }

    fn descriptor_for(agent_id: &str) -> AcpAgentCompatibility {
        AcpCompatibilityRegistry::builtin()
            .unwrap()
            .for_agent(&AgentId::parse(agent_id).unwrap())
            .unwrap()
            .clone()
    }

    #[test]
    fn installed_activation_accepts_a_matching_artifact() {
        let descriptor = descriptor_for("claude");
        let dir = tempfile::tempdir().unwrap();
        let evidence = managed_evidence(
            &descriptor,
            dir.path(),
            "node_modules/adapter/index.js",
            b"launcher",
        );
        let observed = AcpAdapterArtifactObservation::observe(dir.path(), &evidence).unwrap();
        let verified =
            VerifiedAcpAdapterActivation::verify_installed(&descriptor, &evidence, &observed)
                .unwrap();
        assert_eq!(verified.binary_identity(), evidence.entrypoint_hash);
        assert_eq!(
            verified.compatibility_identity().as_str(),
            descriptor.expected_compatibility_identity().as_str()
        );
    }

    #[test]
    fn installed_activation_rejects_a_changed_entrypoint() {
        let descriptor = descriptor_for("claude");
        let dir = tempfile::tempdir().unwrap();
        let evidence = managed_evidence(&descriptor, dir.path(), "adapter.js", b"launcher");
        fs::write(dir.path().join("adapter.js"), b"tampered").unwrap();
        let observed = AcpAdapterArtifactObservation::observe(dir.path(), &evidence).unwrap();
        assert_eq!(
            VerifiedAcpAdapterActivation::verify_installed(&descriptor, &evidence, &observed)
                .unwrap_err()
                .code,
            "acp_adapter_activation_artifact_drift"
        );
    }

    #[test]
    fn installed_activation_rejects_a_runtime_dependency_mismatch() {
        let descriptor = descriptor_for("codex");
        let dir = tempfile::tempdir().unwrap();
        let mut evidence = managed_evidence(&descriptor, dir.path(), "adapter.js", b"launcher");
        evidence
            .runtime_versions
            .insert("@openai/codex".to_string(), "0.1.0".to_string());
        let observed = AcpAdapterArtifactObservation::observe(dir.path(), &evidence).unwrap();
        assert_eq!(
            VerifiedAcpAdapterActivation::verify_installed(&descriptor, &evidence, &observed)
                .unwrap_err()
                .code,
            "acp_adapter_activation_identity_mismatch"
        );
    }

    #[test]
    fn installed_activation_rejects_another_adapters_evidence() {
        let claude = descriptor_for("claude");
        let codex = descriptor_for("codex");
        let dir = tempfile::tempdir().unwrap();
        let evidence = managed_evidence(&claude, dir.path(), "adapter.js", b"launcher");
        let observed = AcpAdapterArtifactObservation::observe(dir.path(), &evidence).unwrap();
        assert_eq!(
            VerifiedAcpAdapterActivation::verify_installed(&codex, &evidence, &observed)
                .unwrap_err()
                .code,
            "acp_adapter_activation_identity_mismatch"
        );
    }

    #[test]
    fn activation_evidence_reader_rejects_missing_and_unknown_schema() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_activation_evidence(dir.path()).unwrap_err().code,
            "acp_adapter_activation_evidence_missing"
        );

        let descriptor = descriptor_for("claude");
        let mut evidence = managed_evidence(&descriptor, dir.path(), "adapter.js", b"launcher");
        evidence.schema_version = ACP_ACTIVATION_EVIDENCE_SCHEMA_VERSION + 1;
        fs::write(
            dir.path().join(ACP_ACTIVATION_EVIDENCE_FILE),
            serde_json::to_vec(&evidence).unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_activation_evidence(dir.path()).unwrap_err().code,
            "acp_adapter_activation_evidence_invalid"
        );
    }
}

use semver::Version;
use vibex_core::{AcpAdapterId, VibexError, VibexResult};

use crate::{
    AcpAdapterHealthReport, AcpAgentCompatibility, AcpBridgeContractAdapterReport,
    AdapterCompatibilityIdentity, BridgeContractEvidenceKind, BridgeContractSummary,
    VerifiedAcpAdapterInstallation,
};

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

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
}

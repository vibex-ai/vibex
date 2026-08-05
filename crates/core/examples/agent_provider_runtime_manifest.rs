use std::error::Error;

use serde::Serialize;
use vibex_core::{
    AgentProviderRolloutManifestEntry, agent_provider_rollout_manifest, validate_rollout_manifest,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestExport {
    schema_version: &'static str,
    entries: Vec<AgentProviderRolloutManifestEntry>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let entries = agent_provider_rollout_manifest()?;
    validate_rollout_manifest(&entries)?;
    serde_json::to_writer_pretty(
        std::io::stdout(),
        &ManifestExport {
            schema_version: "agent-provider-runtime-manifest.v1",
            entries,
        },
    )?;
    println!();
    Ok(())
}

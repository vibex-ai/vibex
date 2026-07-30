use std::path::PathBuf;

use vibex_diagnostics::run_diagnostic_bundle_smoke;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = PathBuf::from("target")
        .join("stage0")
        .join("vibex-diagnostic-bundle-smoke.db");
    let output_path = PathBuf::from("target")
        .join("stage0")
        .join("diagnostic-bundle-smoke.json");
    let result = run_diagnostic_bundle_smoke(&db_path, &output_path)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

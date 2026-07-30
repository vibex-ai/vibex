use std::path::PathBuf;

use vibex_db::{run_smoke, stage0_smoke_database_path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(stage0_smoke_database_path)?;
    let result = run_smoke(&path)?;

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

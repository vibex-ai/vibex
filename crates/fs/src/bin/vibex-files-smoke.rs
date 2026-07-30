use std::path::PathBuf;

use vibex_fs::run_files_smoke;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target").join("stage0").join("files-smoke"));
    let result = run_files_smoke(root)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

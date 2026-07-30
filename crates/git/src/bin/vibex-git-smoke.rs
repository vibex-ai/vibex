use std::path::PathBuf;

use vibex_git::git_status;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let status = git_status(repo_path)?;

    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}

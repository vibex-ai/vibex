use vibex_backup::run_backup_restore_smoke;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let result = run_backup_restore_smoke()?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

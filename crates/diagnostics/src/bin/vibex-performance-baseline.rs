use vibex_diagnostics::run_performance_baseline;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let result = run_performance_baseline()?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if result.has_blocker() {
        std::process::exit(1);
    }
    Ok(())
}

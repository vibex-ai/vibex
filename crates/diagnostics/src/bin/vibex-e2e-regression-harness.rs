use vibex_diagnostics::{assert_e2e_regression_output_redacted, run_e2e_regression_harness};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let result = run_e2e_regression_harness().await?;
    let json = serde_json::to_string_pretty(&result)?;
    assert_e2e_regression_output_redacted(&json)?;
    println!("{json}");
    if result.has_blocker() {
        std::process::exit(1);
    }
    Ok(())
}

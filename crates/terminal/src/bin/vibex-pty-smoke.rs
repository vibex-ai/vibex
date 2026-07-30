use vibex_terminal::run_pty_smoke;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let result = run_pty_smoke()?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

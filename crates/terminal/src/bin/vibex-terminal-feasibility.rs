use vibex_terminal::run_terminal_feasibility;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let run = run_terminal_feasibility()?;
    println!("{}", serde_json::to_string_pretty(&run)?);
    Ok(())
}

use vibex_agent_acp::run_claude_agent_acp_smoke;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prompt = std::env::args().nth(1);
    let result = run_claude_agent_acp_smoke(prompt).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

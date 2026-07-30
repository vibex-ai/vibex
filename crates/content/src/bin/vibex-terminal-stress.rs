use std::{env, fs, path::PathBuf};

use vibex_content::run_terminal_stress;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut output = None::<PathBuf>;
    let mut seconds = None::<u64>;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--output" => output = arguments.next().map(PathBuf::from),
            "--soak-seconds" => seconds = arguments.next().and_then(|value| value.parse().ok()),
            "--quick" => seconds = Some(0),
            value => return Err(format!("unknown argument: {value}").into()),
        }
    }
    let report = run_terminal_stress(seconds)?;
    let serialized = serde_json::to_string_pretty(&report)?;
    if let Some(output) = output {
        fs::write(output, format!("{serialized}\n"))?;
    } else {
        println!("{serialized}");
    }
    if report.status != "passed" {
        return Err("terminal stress report failed".into());
    }
    Ok(())
}

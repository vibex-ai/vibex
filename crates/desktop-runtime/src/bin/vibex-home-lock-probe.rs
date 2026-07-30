use std::path::PathBuf;
use std::time::Duration;

use vibex_desktop_runtime::DesktopHomeLock;

fn main() {
    let mut args = std::env::args().skip(1);
    let home = args.next().map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("usage: vibex-home-lock-probe <home> [hold-ms]");
        std::process::exit(2);
    });
    let hold_ms = args
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let _lock =
        DesktopHomeLock::acquire(&home, "dev.vibex.desktop.lock-probe").unwrap_or_else(|error| {
            eprintln!("{}", error.code);
            std::process::exit(3);
        });
    println!("locked");
    if hold_ms > 0 {
        std::thread::sleep(Duration::from_millis(hold_ms));
    }
}

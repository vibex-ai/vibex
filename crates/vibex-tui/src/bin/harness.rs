//! A deliberately minimal host for the PTY tests.
//!
//! The PTY layer exists to prove things a `TestBackend` cannot: that the real
//! binary enters raw mode, paints a first frame, restores the terminal on exit,
//! and stays completely silent when nothing is happening. Driving those
//! through `vibex` proper would start a real runtime and make the test depend
//! on the machine's state, so the harness pairs the real event loop with a
//! backend that is honest about being disconnected.
//!
//! It can also behave like the one thing `vibex` does that the harness does
//! not: share the process with a runtime that reports progress on `stderr`.

use vibex_backend::DisconnectedBackend;
use vibex_tui::{ExitReason, SeatKind, TuiOptions};

/// Makes the harness write diagnostics to `stderr` while the interface runs.
const STRAY_STDERR_ENVIRONMENT: &str = "VIBEX_TUI_HARNESS_STRAY_STDERR";

fn main() {
    if std::env::var_os(STRAY_STDERR_ENVIRONMENT).is_some() {
        // The authority seat boots the runtime in this process, and the
        // runtime's startup stages finish on background tasks after the first
        // frame is painted. A thread writing on the same schedule is the
        // faithful stand-in for that.
        std::thread::spawn(|| {
            for index in 0..20 {
                eprintln!("vibex-startup: stage-begin stage=harness_stray_{index}");
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
    }

    let options = TuiOptions {
        seat: SeatKind::Remote,
        theme_id: Some("vibex-dark".to_string()),
        ..TuiOptions::default()
    };
    match vibex_tui::run(DisconnectedBackend::facade(), options) {
        Ok(ExitReason::UserQuit) | Ok(ExitReason::ConnectionLost) => {}
        Err(error) => {
            // Printed outside the alternate screen: `run` restores the
            // terminal before returning, so this reaches the real stdout.
            eprintln!("{}: {}", error.code, error.message);
            std::process::exit(1);
        }
    }
}

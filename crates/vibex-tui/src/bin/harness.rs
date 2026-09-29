//! A deliberately minimal host for the PTY tests.
//!
//! The PTY layer exists to prove things a `TestBackend` cannot: that the real
//! binary enters raw mode, paints a first frame, restores the terminal on exit,
//! and stays completely silent when nothing is happening. Driving those
//! through `vibex` proper would start a real runtime and make the test depend
//! on the machine's state, so the harness pairs the real event loop with a
//! backend that is honest about being disconnected.

use vibex_backend::DisconnectedBackend;
use vibex_tui::{ExitReason, SeatKind, TuiOptions};

fn main() {
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

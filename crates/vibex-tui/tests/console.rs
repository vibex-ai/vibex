//! The `stderr` diversion, exercised in its own process.
//!
//! This is deliberately not a unit test: diverting `stderr` is process-wide,
//! and the libtest process runs tests in parallel, so a sibling test's failure
//! message could be swallowed while the diversion is up.
//!
//! The end-to-end form of the same contract — a byte written while the
//! interface owns the screen never reaches the terminal — lives in `pty.rs`.

use std::io::Write;

/// Writes to the descriptor itself.
///
/// `eprintln!` is not enough: libtest installs a thread-local output capture
/// that the macro consults, so a captured line never reaches `stderr` at all
/// and the diversion would look like it worked when it was never exercised.
fn write_to_stderr(text: &str) {
    let mut stderr = std::io::stderr();
    stderr
        .write_all(text.as_bytes())
        .expect("stderr accepts a write");
    stderr.flush().expect("stderr flushes");
}

/// The spill file the client picks when `VIBEX_TUI_LOG` is unset.
fn default_spill_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("vibex-tui-{}.log", std::process::id()))
}

#[test]
fn diverted_output_is_captured_and_the_terminal_is_restored() {
    let path = default_spill_path();
    let _ = std::fs::remove_file(&path);

    vibex_tui::console::divert_stderr().expect("the spill file is writable");
    write_to_stderr("console-test-marker-1\n");
    vibex_tui::console::restore_stderr();

    let (captured_path, bytes) = vibex_tui::console::captured().expect("output was captured");
    assert!(bytes > 0, "the spill file recorded no bytes");
    let spilled = std::fs::read_to_string(&captured_path).expect("the spill file exists");
    assert!(spilled.contains("console-test-marker-1"), "{spilled}");

    // A terminal hand-off (`$EDITOR`) releases the guard and re-acquires it. The
    // second diversion has to append to the same file, not start a new one.
    vibex_tui::console::divert_stderr().expect("the spill file is re-opened");
    write_to_stderr("console-test-marker-2\n");
    vibex_tui::console::restore_stderr();

    let spilled = std::fs::read_to_string(&captured_path).expect("the spill file survives");
    assert!(spilled.contains("console-test-marker-1"), "{spilled}");
    assert!(spilled.contains("console-test-marker-2"), "{spilled}");

    // Only the default file belongs to this test; a developer-set
    // `VIBEX_TUI_LOG` is left alone.
    if captured_path == path {
        let _ = std::fs::remove_file(&path);
    }
}

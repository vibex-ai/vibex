//! Terminal ownership: entering the alternate screen, handing the terminal to
//! another program, and — most importantly — always giving it back.
//!
//! Three hand-off shapes are needed and each has a distinct failure mode:
//!
//! * [`TerminalGuard`] restores on `Drop` **and** installs a panic hook, so a
//!   panic inside a render or a backend call cannot leave the user in raw mode
//!   with an invisible cursor.
//! * [`with_terminal_restored`] runs a child that needs a real TTY (`$EDITOR`,
//!   `git commit`) and re-enters the TUI afterwards even if the child failed.
//! * Restoration is best-effort across *all* steps: the code deliberately keeps
//!   going after the first error instead of returning early, because a partial
//!   restore is still better than none.
//!
//! The clipboard goes through OSC 52, so the client never links an
//! X11/Wayland clipboard crate and never touches the user's display server.

use std::io::{self, IsTerminal, Write};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use vibex_backend::{BackendError, BackendResult};

/// Whether both stdin and stdout are interactive terminals.
///
/// A TUI that cannot ask a question has to fail loudly instead of half-drawing;
/// the caller uses this to print a plain error and exit non-zero.
pub fn is_interactive_terminal() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Restores the terminal. Idempotent, and safe to call from a panic hook.
pub fn restore_terminal() {
    // Every step is attempted even if an earlier one failed: leaving the
    // cursor hidden is worse than a failed mouse-mode reset.
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(
        stdout,
        LeaveAlternateScreen,
        crossterm::cursor::Show,
        DisableMouseCapture,
        DisableBracketedPaste
    );
    let _ = stdout.flush();
}

/// Owns the terminal for the lifetime of the interface.
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    /// Enter raw mode and the alternate screen.
    pub fn enter() -> BackendResult<Self> {
        enable_raw_mode()
            .map_err(|error| BackendError::failed("tui_terminal_unavailable", error.to_string()))?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            crossterm::cursor::Hide
        )
        .map_err(|error| BackendError::failed("tui_terminal_unavailable", error.to_string()))?;
        stdout
            .flush()
            .map_err(|error| BackendError::failed("tui_terminal_unavailable", error.to_string()))?;
        install_panic_hook();
        Ok(Self { active: true })
    }

    /// Hand the terminal back early. Dropping afterwards is a no-op.
    pub fn release(&mut self) {
        if self.active {
            restore_terminal();
            self.active = false;
        }
    }

    /// Re-enter after a child program has finished with the terminal.
    pub fn reacquire(&mut self) -> BackendResult<()> {
        if self.active {
            return Ok(());
        }
        *self = Self::enter()?;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.release();
    }
}

fn install_panic_hook() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Restore first: the previous hook may print, and printing into the
            // alternate screen is exactly what we are trying to avoid.
            restore_terminal();
            previous(info);
        }));
    });
}

/// Run `f` with the terminal handed back to the operating system.
///
/// Used for `$EDITOR` and for interactive git commands. The guard is released
/// before the callback and re-acquired afterwards, so a panic or an `Err`
/// inside `f` still leaves the TUI usable.
pub fn with_terminal_restored<T>(
    guard: &mut TerminalGuard,
    f: impl FnOnce() -> T,
) -> Result<T, BackendError> {
    guard.release();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    // Re-acquire regardless of how the child ended.
    let reacquire = guard.reacquire();
    match result {
        Ok(value) => {
            reacquire?;
            Ok(value)
        }
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|value| (*value).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "child program panicked".to_string());
            // Report both failures when re-acquisition also failed, so the
            // user is not told only half of what went wrong.
            match reacquire {
                Ok(()) => Err(BackendError::failed("tui_child_failed", message)),
                Err(error) => Err(BackendError::failed(
                    "tui_child_failed",
                    format!(
                        "{message}; the interface could not be restored: {}",
                        error.message
                    ),
                )),
            }
        }
    }
}

/// Replace `exec`-style hand-off: run a program and wait for it.
pub fn run_child(program: &str, arguments: &[String]) -> BackendResult<i32> {
    let status = std::process::Command::new(program)
        .args(arguments)
        .status()
        .map_err(|error| {
            BackendError::failed(
                "tui_child_spawn_failed",
                format!("could not run {program}: {error}"),
            )
        })?;
    Ok(status.code().unwrap_or(-1))
}

/// Open `$EDITOR` (or `$VISUAL`) on `body` and return the edited text.
pub fn edit_in_editor(title: &str, body: &str) -> BackendResult<Option<String>> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "vi".to_string());

    let mut path = std::env::temp_dir();
    path.push(format!(
        "vibex-tui-{}-{}.md",
        sanitize(title),
        std::process::id()
    ));
    std::fs::write(&path, body)
        .map_err(|error| BackendError::failed("tui_editor_scratch_failed", error.to_string()))?;

    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi").to_string();
    let mut arguments = parts.map(str::to_string).collect::<Vec<_>>();
    arguments.push(path.to_string_lossy().to_string());
    let status = run_child(&program, &arguments);
    let edited = std::fs::read_to_string(&path).ok();
    let _ = std::fs::remove_file(&path);
    status?;
    Ok(edited)
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .take(24)
        .collect()
}

/// Copy `text` to the system clipboard through OSC 52.
///
/// OSC 52 is understood by tmux, kitty, iTerm2, WezTerm, Windows Terminal and
/// most other modern terminals, and it works over SSH — which a native
/// clipboard library would not.
pub fn copy_to_clipboard(text: &str) -> BackendResult<()> {
    use base64_encode::encode;
    let encoded = encode(text.as_bytes());
    let sequence = format!("\x1b]52;c;{encoded}\x07");
    let mut stdout = io::stdout();
    stdout
        .write_all(sequence.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| BackendError::failed("tui_clipboard_unavailable", error.to_string()))
}

/// Minimal base64 so the client does not pull a dependency for one call.
mod base64_encode {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(input: &[u8]) -> String {
        let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let triple = (b0 << 16) | (b1 << 8) | b2;
            output.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
            output.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
            output.push(if chunk.len() > 1 {
                ALPHABET[((triple >> 6) & 0x3f) as usize] as char
            } else {
                '='
            });
            output.push(if chunk.len() > 2 {
                ALPHABET[(triple & 0x3f) as usize] as char
            } else {
                '='
            });
        }
        output
    }
}

/// The message printed when stdout is not a terminal.
pub fn non_interactive_help() -> &'static str {
    "vibex tui needs an interactive terminal.\n\
     Run it from a terminal, or use a non-interactive entry point such as\n\
     `vibex --help`."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_reference_vectors() {
        assert_eq!(base64_encode::encode(b""), "");
        assert_eq!(base64_encode::encode(b"f"), "Zg==");
        assert_eq!(base64_encode::encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode::encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode::encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode::encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode::encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_cjk_and_emoji() {
        let value = "中文 👋";
        let encoded = base64_encode::encode(value.as_bytes());
        assert!(encoded.len().is_multiple_of(4));
        // Decoding is not implemented here, so assert the length relationship:
        // 3 bytes → 4 characters.
        assert_eq!(encoded.len(), value.len().div_ceil(3) * 4);
    }

    #[test]
    fn scratch_filenames_are_sanitized() {
        assert_eq!(sanitize("Hello World!"), "Hello-World-");
        assert_eq!(sanitize("../../etc/passwd"), "------etc-passwd");
        assert!(sanitize(&"x".repeat(100)).len() <= 24);
    }

    #[test]
    fn a_missing_editor_binary_reports_a_spawn_failure() {
        let error = run_child("definitely-not-a-real-binary-xyz", &[]).unwrap_err();
        assert_eq!(error.code, "tui_child_spawn_failed");
    }

    #[test]
    fn running_a_real_child_reports_its_exit_code() {
        let code = run_child("true", &[]).expect("true exists");
        assert_eq!(code, 0);
    }

    #[test]
    fn restoration_is_idempotent() {
        // The guard is not entered here (there is no tty in the test harness),
        // but releasing a never-entered guard must be a no-op rather than a
        // panic.
        let mut guard = TerminalGuard { active: false };
        guard.release();
        guard.release();
    }

    #[test]
    fn non_interactive_help_points_at_a_working_command() {
        assert!(non_interactive_help().contains("--help"));
    }
}

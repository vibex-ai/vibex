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
//! Taking the screen also means taking `stderr`: while the guard is active, the
//! process's own diagnostics are diverted into a spill file by [`crate::console`]
//! so that a stray `eprintln!` from anywhere in the process cannot land in a
//! frame. [`report_captured_stderr`] says where they went once the screen is
//! back.
//!
//! The clipboard goes through OSC 52, so the client never links an
//! X11/Wayland clipboard crate and never touches the user's display server.
//!
//! Taking the screen is also where the keyboard protocol is asked for, because
//! without it a terminal cannot report a chord like `Shift+Enter` at all — see
//! [`push_keyboard_enhancement`].

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use vibex_backend::{BackendError, BackendResult};

/// The keyboard-protocol flags the client asks a capable terminal for.
///
/// `DISAMBIGUATE_ESCAPE_CODES` is what makes `Shift+Enter` a key of its own: a
/// terminal that has not been asked for the protocol sends the same carriage
/// return for `Enter` and `Shift+Enter`, so "break the line" and "send" arrive
/// as one event and only one of them can win.
///
/// `REPORT_ALTERNATE_KEYS` is what keeps *text* intact under it. The protocol
/// reports a shifted key as its unshifted code plus a `Shift` modifier, and the
/// character the reader actually pressed travels only in the alternate keycode;
/// without the flag `Shift+2` would type `2` rather than `@`.
const KEYBOARD_ENHANCEMENT: KeyboardEnhancementFlags =
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        .union(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS);

/// Whether those flags are currently pushed onto the terminal's stack.
///
/// A terminal that does not implement the protocol ignores both sequences, so
/// this exists only to keep the pop balanced with the push: a `Pop` a terminal
/// never had pushed is not an error, but it is also not ours to send when a
/// parent program may have flags of its own.
static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);

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
    // The keyboard protocol is popped first and only when it was pushed, so the
    // program that gets the terminal back reads the keys it expects rather than
    // one it never asked to have reported this way.
    if KEYBOARD_ENHANCED.swap(false, Ordering::SeqCst) {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        stdout,
        LeaveAlternateScreen,
        crossterm::cursor::Show,
        DisableMouseCapture,
        DisableBracketedPaste
    );
    let _ = stdout.flush();
    // `stderr` comes back last: whatever is printed after the screen is
    // restored — a panic message, a late diagnostic — is visible again.
    crate::console::restore_stderr();
}

/// Ask the terminal to report keys the legacy encoding cannot distinguish.
///
/// Deliberately optimistic. The two sequences are defined to be ignored by a
/// terminal that does not implement the protocol, and the question can only be
/// asked through the same input queue the interface is about to read: asking
/// first would stall startup on every terminal that answers the companion
/// device-attributes query but not this one, for an answer that changes nothing
/// but the wording of the key bar. A terminal that ignores the push keeps its
/// legacy keys, where `Ctrl+J` — a line feed, which every terminal sends as a
/// byte of its own — is the newline chord.
fn push_keyboard_enhancement() {
    let mut stdout = io::stdout();
    let pushed = execute!(stdout, PushKeyboardEnhancementFlags(KEYBOARD_ENHANCEMENT));
    if pushed.is_ok() {
        KEYBOARD_ENHANCED.store(true, Ordering::SeqCst);
    }
}

/// Report the diagnostics captured while the interface owned the terminal.
///
/// They are kept off the screen by design, so naming the spill file is what
/// makes "diverted" mean something other than "lost". Callers run this after
/// [`TerminalGuard::release`], when `stderr` is a terminal again.
pub fn report_captured_stderr() {
    if let Some((path, bytes)) = crate::console::captured() {
        eprintln!(
            "vibex: {bytes} bytes of in-process diagnostics were captured in {}",
            path.display()
        );
    }
}

/// Owns the terminal for the lifetime of the interface.
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    /// Enter raw mode and the alternate screen.
    pub fn enter() -> BackendResult<Self> {
        // The diversion goes up before the alternate screen does: output
        // written in between would still land in the frame.
        let _ = crate::console::divert_stderr();
        let entered = (|| -> io::Result<()> {
            enable_raw_mode()?;
            let mut stdout = io::stdout();
            execute!(
                stdout,
                EnterAlternateScreen,
                EnableMouseCapture,
                EnableBracketedPaste,
                crossterm::cursor::Hide
            )?;
            stdout.flush()
        })();
        if let Err(error) = entered {
            // A half-entered terminal is worse than none, so give back whatever
            // was taken — including `stderr` — before reporting the failure.
            restore_terminal();
            return Err(BackendError::failed(
                "tui_terminal_unavailable",
                error.to_string(),
            ));
        }
        // Asked for after the screen is taken, and a terminal that will not
        // answer is not a terminal that cannot run the interface.
        push_keyboard_enhancement();
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

    let scratch = EditorScratch::create(title, body)
        .map_err(|error| BackendError::failed("tui_editor_scratch_failed", error.to_string()))?;

    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi").to_string();
    let mut arguments = parts.map(str::to_string).collect::<Vec<_>>();
    arguments.push(scratch.path.to_string_lossy().to_string());
    let status = run_child(&program, &arguments);
    let edited = std::fs::read_to_string(&scratch.path).ok();
    status?;
    Ok(edited)
}

/// Each editor invocation owns its scratch file, even when sessions use the
/// same title. Cleanup follows that invocation on success and failure alike.
struct EditorScratch {
    path: std::path::PathBuf,
}

impl EditorScratch {
    fn create(title: &str, body: &str) -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "vibex-tui-{}-{}-{}.md",
            sanitize(title),
            std::process::id(),
            vibex_core::RequestId::new()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        let scratch = Self { path };
        let written = file.write_all(body.as_bytes());
        // Close before cleanup on a failed write, including on Windows.
        drop(file);
        written?;
        Ok(scratch)
    }
}

impl Drop for EditorScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
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
    use base64::encode;
    let encoded = encode(text.as_bytes());
    let sequence = format!("\x1b]52;c;{encoded}\x07");
    let mut stdout = io::stdout();
    stdout
        .write_all(sequence.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| BackendError::failed("tui_clipboard_unavailable", error.to_string()))
}

/// How long a clipboard helper may take before it is killed.
///
/// These programs talk to a clipboard owner that may be gone; a request that
/// has not answered in a moment is not going to. The budget has to fit the
/// slowest helper, which is the shell on macOS and Windows that has to start an
/// interpreter first — and it is spent on a worker thread, so the interface
/// never waits for it.
const CLIPBOARD_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2_000);

/// Image types the composer can attach, in the order a clipboard is asked for
/// them. PNG first: a screenshot is always one.
const IMAGE_TYPES: [&str; 5] = [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
];

/// Read an image off the system clipboard, if there is one.
///
/// A terminal gives a client no way to ask for pixels, so this shells out to
/// whichever clipboard tool the desktop provides: `wl-paste` on Wayland,
/// `xclip` under X11, `pngpaste` or `osascript` on macOS, PowerShell on
/// Windows. A machine with none of them simply has no clipboard images, which
/// is reported as `None` rather than an error — the reader can still attach a
/// file by path.
///
/// The clipboard decides what kind of image it holds — a screenshot is a PNG,
/// a picture copied out of a browser or a photo library is usually a JPEG — so
/// the offered types are listed first and the bytes are read back under the
/// type the owner offers. Asking for PNG and nothing else finds nothing on half
/// the clipboards it meets.
pub fn read_clipboard_image() -> Option<(String, Vec<u8>)> {
    // Wayland. A clipboard that lists no image is not a reason to stop: an X11
    // application's copy may only be visible through the compatibility layer.
    let wayland = list_clipboard_types("wl-paste", &["--list-types"])
        .and_then(|listing| first_image_type(&listing));
    if let Some(mime) = wayland
        && let Some(bytes) = run_clipboard_command(
            "wl-paste",
            &[
                "--no-newline".to_string(),
                "--type".to_string(),
                mime.clone(),
            ],
        )
    {
        return Some((mime, bytes));
    }
    // X11.
    let x11 = list_clipboard_types("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"])
        .and_then(|listing| first_image_type(&listing));
    if let Some(mime) = x11
        && let Some(bytes) = run_clipboard_command(
            "xclip",
            &[
                "-selection".to_string(),
                "clipboard".to_string(),
                "-t".to_string(),
                mime.clone(),
                "-o".to_string(),
            ],
        )
    {
        return Some((mime, bytes));
    }
    // macOS: `pngpaste` when it is installed, otherwise the system script host,
    // which can write the clipboard's PNG to a file and needs no install.
    if let Some(bytes) = run_clipboard_command("pngpaste", &["-".to_string()]) {
        return Some(("image/png".to_string(), bytes));
    }
    if let Some(bytes) = read_clipboard_image_via_file(&macos_clipboard_script()) {
        return Some(("image/png".to_string(), bytes));
    }
    // Windows.
    let script = windows_clipboard_script(&clipboard_temp_path())?;
    read_clipboard_image_via_file(&[
        "powershell".to_string(),
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-Command".to_string(),
        script,
    ])
    .map(|bytes| ("image/png".to_string(), bytes))
}

/// Read the clipboard's text, if it has any.
///
/// The reader's own paste gesture: a terminal in bracketed-paste mode never
/// sends the key, so this exists for the ones that do, and for a client that
/// wants to paste without a terminal's help.
pub fn read_clipboard_text() -> Option<String> {
    for (program, args) in [
        ("wl-paste", vec!["--no-newline"]),
        ("xclip", vec!["-selection", "clipboard", "-o"]),
        ("pbpaste", vec![]),
    ] {
        let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
        if let Some(bytes) = run_clipboard_command(program, &args)
            && let Ok(text) = String::from_utf8(bytes)
        {
            return Some(text);
        }
    }
    None
}

/// A path in the temp directory for a clipboard helper to write into.
fn clipboard_temp_path() -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("vibex-clipboard-{}.png", std::process::id()));
    path
}

/// The first image type a clipboard offers, in [`IMAGE_TYPES`] order.
///
/// Pure, so the parsing is testable without a clipboard.
fn first_image_type(listing: &str) -> Option<String> {
    let offered = listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    IMAGE_TYPES
        .iter()
        .find(|wanted| offered.contains(wanted))
        .map(|mime| (*mime).to_string())
}

/// The clipboard's offered types, one per line, or `None` without a helper.
fn list_clipboard_types(program: &str, args: &[&str]) -> Option<String> {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let bytes = run_clipboard_command(program, &args)?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Run a helper that writes the clipboard image to
/// [`clipboard_temp_path`], then read and remove that file.
fn read_clipboard_image_via_file(args: &[String]) -> Option<Vec<u8>> {
    let path = clipboard_temp_path();
    let _ = std::fs::remove_file(&path);
    // `args` is built here, never by the reader, so the path in the script
    // cannot carry a quote out of it.
    let args = args
        .iter()
        .map(|arg| arg.replace("{path}", &path.display().to_string()))
        .collect::<Vec<_>>();
    let ran = run_clipboard_command(&args[0], &args[1..]);
    let bytes = std::fs::read(&path).ok();
    let _ = std::fs::remove_file(&path);
    ran?;
    bytes.filter(|bytes| !bytes.is_empty())
}

/// AppleScript that writes the clipboard's PNG to `{path}`.
fn macos_clipboard_script() -> Vec<String> {
    vec![
        "osascript".to_string(),
        "-e".to_string(),
        "set outFile to POSIX file \"{path}\"".to_string(),
        "-e".to_string(),
        "set theImage to (the clipboard as «class PNGf»)".to_string(),
        "-e".to_string(),
        "set fh to open for access outFile with write permission".to_string(),
        "-e".to_string(),
        "set eof fh to 0".to_string(),
        "-e".to_string(),
        "write theImage to fh".to_string(),
        "-e".to_string(),
        "close access fh".to_string(),
    ]
}

/// PowerShell that saves the clipboard image to `{path}`.
fn windows_clipboard_script(path: &std::path::Path) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    Some(format!(
        "Add-Type -AssemblyName System.Windows.Forms;          $image = [System.Windows.Forms.Clipboard]::GetImage();          if ($image) {{ $image.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png) }}",
        path.display()
    ))
}

/// Run one clipboard helper, with a deadline and a size cap.
fn run_clipboard_command(program: &str, args: &[String]) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // The pipe fills long before an image is done, so a reader thread drains it
    // while the parent watches the clock.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let deadline = std::time::Instant::now() + CLIPBOARD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let bytes = reader.join().unwrap_or_default();
                if !status.success() || bytes.is_empty() {
                    return None;
                }
                return Some(bytes);
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            // A helper that overruns is killed: the reader thread sees EOF and
            // ends by itself.
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

/// Base64 for a data URL, exposed so attachments do not grow a second copy.
pub fn encode_base64(input: &[u8]) -> String {
    base64::encode(input)
}

/// Base64 for the terminal's own clipboard, which carries its answer in it.
///
/// `None` for anything that is not valid base64, padding included: the module
/// below is deliberately strict, because a decoder that skips what it does not
/// understand turns a corrupted packet into an apparently valid one.
pub(crate) fn decode_base64(input: &str) -> Option<Vec<u8>> {
    base64::decode(input)
}

/// Minimal base64 so the client does not pull a dependency for two calls.
mod base64 {
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

    /// Decode `input`, refusing anything that is not whole padded base64.
    pub fn decode(input: &str) -> Option<Vec<u8>> {
        let bytes = input.as_bytes();
        if !bytes.len().is_multiple_of(4) {
            return None;
        }
        let mut output = Vec::with_capacity(bytes.len() / 4 * 3);
        for chunk in bytes.chunks(4) {
            let mut values = [0u32; 4];
            let mut padding = 0usize;
            for (index, byte) in chunk.iter().enumerate() {
                if *byte == b'=' {
                    // Padding is only ever the tail of a quantum.
                    if index < 2 {
                        return None;
                    }
                    padding += 1;
                    continue;
                }
                if padding > 0 {
                    // Data behind padding is not a padded quantum.
                    return None;
                }
                values[index] = u32::try_from(ALPHABET.iter().position(|it| it == byte)?).ok()?;
            }
            let triple = (values[0] << 18) | (values[1] << 12) | (values[2] << 6) | values[3];
            output.push(((triple >> 16) & 0xff) as u8);
            if padding < 2 {
                output.push(((triple >> 8) & 0xff) as u8);
            }
            if padding < 1 {
                output.push((triple & 0xff) as u8);
            }
        }
        Some(output)
    }
}

/// The message printed when stdout is not a terminal.
pub fn non_interactive_help() -> &'static str {
    "vibex tui needs an interactive terminal.\n\
     Run it from a terminal, or use a non-interactive entry point such as\n\
     `vibex --help`."
}

#[cfg(test)]
mod clipboard_tests {
    use super::*;

    #[test]
    fn the_offered_image_type_is_the_one_that_is_read_back() {
        // A screenshot is a PNG, a picture from a browser is usually a JPEG,
        // and a client that only ever asks for PNG finds nothing on half the
        // clipboards it meets.
        // PNG is preferred when the clipboard offers it, and JPEG is taken when
        // that is all there is.
        let listing = "text/plain\nTEXT\nimage/jpeg\nimage/png\n";
        assert_eq!(first_image_type(listing).as_deref(), Some("image/png"));
        assert_eq!(
            first_image_type("text/plain\nimage/jpeg").as_deref(),
            Some("image/jpeg")
        );
        assert_eq!(
            first_image_type("image/png\nimage/jpeg").as_deref(),
            Some("image/png")
        );
        assert_eq!(first_image_type("text/plain\nTEXT\n"), None);
        assert_eq!(first_image_type(""), None);
        // A type the composer cannot attach is not a type it asks for.
        assert_eq!(first_image_type("image/tiff\nimage/svg+xml"), None);
        assert_eq!(
            first_image_type("  image/webp  \n"),
            Some("image/webp".to_string())
        );
    }

    #[test]
    fn the_clipboard_scripts_name_the_file_they_write() {
        let macos = macos_clipboard_script();
        assert_eq!(macos[0], "osascript");
        assert!(macos.iter().any(|part| part.contains("{path}")));
        assert!(macos.iter().any(|part| part.contains("PNGf")));
        // The Windows script only exists on Windows, where it can be run.
        assert_eq!(
            windows_clipboard_script(std::path::Path::new("x.png")).is_some(),
            cfg!(windows)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_reference_vectors() {
        assert_eq!(base64::encode(b""), "");
        assert_eq!(base64::encode(b"f"), "Zg==");
        assert_eq!(base64::encode(b"fo"), "Zm8=");
        assert_eq!(base64::encode(b"foo"), "Zm9v");
        assert_eq!(base64::encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64::encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64::encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_cjk_and_emoji() {
        let value = "中文 👋";
        let encoded = base64::encode(value.as_bytes());
        assert!(encoded.len().is_multiple_of(4));
        // The terminal's own clipboard answers in base64, so the two halves
        // have to be each other's inverse for every width of input.
        assert_eq!(decode_base64(&encoded).as_deref(), Some(value.as_bytes()));
    }

    #[test]
    fn base64_decoding_refuses_what_is_not_base64() {
        assert_eq!(decode_base64("").as_deref(), Some(&b""[..]));
        assert_eq!(decode_base64("Zg==").as_deref(), Some(&b"f"[..]));
        assert_eq!(decode_base64("Zm8=").as_deref(), Some(&b"fo"[..]));
        assert_eq!(decode_base64("Zm9v").as_deref(), Some(&b"foo"[..]));
        // A packet is decoded as it arrives, so a truncated quantum must not
        // pass for a short one.
        assert_eq!(decode_base64("Zg="), None);
        assert_eq!(decode_base64("Z"), None);
        // Padding is only ever the tail of a quantum.
        assert_eq!(decode_base64("=Zg="), None);
        assert_eq!(decode_base64("Z=g="), None);
        assert_eq!(decode_base64("Zg==Zg==").as_deref(), Some(&b"ff"[..]));
        // Characters outside the alphabet are refused rather than skipped.
        assert_eq!(decode_base64("Zm9\n"), None);
        assert_eq!(decode_base64("Zm9-"), None);
    }

    #[test]
    fn scratch_filenames_are_sanitized() {
        assert_eq!(sanitize("Hello World!"), "Hello-World-");
        assert_eq!(sanitize("../../etc/passwd"), "------etc-passwd");
        assert!(sanitize(&"x".repeat(100)).len() <= 24);
    }

    #[test]
    fn simultaneous_editors_with_the_same_title_own_separate_scratch_files() {
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| EditorScratch::create("New session", "first draft"));
            let second = scope.spawn(|| EditorScratch::create("New session", "second draft"));
            (
                first.join().unwrap().unwrap(),
                second.join().unwrap().unwrap(),
            )
        });
        assert_ne!(first.path, second.path);
        assert_eq!(std::fs::read_to_string(&first.path).unwrap(), "first draft");
        assert_eq!(
            std::fs::read_to_string(&second.path).unwrap(),
            "second draft"
        );

        std::fs::write(&first.path, "edited first draft").unwrap();
        let first_path = first.path.clone();
        drop(first);
        assert!(!first_path.exists());
        assert_eq!(
            std::fs::read_to_string(&second.path).unwrap(),
            "second draft"
        );

        let second_path = second.path.clone();
        drop(second);
        assert!(!second_path.exists());
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
    fn the_keyboard_protocol_asks_for_disambiguation_and_alternate_keys() {
        // The exact wire form, because it is the whole fix: without the push a
        // terminal has no way to report `Shift+Enter` as anything but `Enter`.
        // Disambiguate is bit 1 and alternate keys bit 4.
        use crossterm::Command;

        let mut push = String::new();
        PushKeyboardEnhancementFlags(KEYBOARD_ENHANCEMENT)
            .write_ansi(&mut push)
            .expect("the push has an ANSI form");
        assert_eq!(push, "\u{1b}[>5u");

        let mut pop = String::new();
        PopKeyboardEnhancementFlags
            .write_ansi(&mut pop)
            .expect("the pop has an ANSI form");
        assert_eq!(pop, "\u{1b}[<1u");
    }

    #[test]
    fn non_interactive_help_points_at_a_working_command() {
        assert!(non_interactive_help().contains("--help"));
    }
}

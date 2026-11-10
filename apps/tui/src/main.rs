//! `vibex` — the standalone character-grid client.
//!
//! ```text
//! vibex                      attach to (or start) the runtime for this home
//! vibex tui                  same as above
//! vibex connect <link|code>  pair with a runtime and attach to it
//! vibex status               report which mode this home would use
//! vibex computer <command>   drive the desktop through a running runtime
//! vibex --help               usage
//! vibex --version            version
//! ```
//!
//! The binary is deliberately thin: seat resolution lives in [`seat`] and the
//! interface itself lives in `vibex-tui`, which never learns how the facade was
//! built.

use std::process::ExitCode;

use vibex_client::seat::{Seat, SeatError, SeatRequest};
use vibex_tui::{ExitReason, SeatKind, TuiOptions};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match run(arguments) {
        Ok(code) => code,
        Err(Failure::Usage(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(Failure::Message(text)) => {
            // Written to stderr so a piped invocation still gets clean stdout.
            eprintln!("{text}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug)]
enum Failure {
    /// Help or version output: not an error.
    Usage(String),
    Message(String),
}

impl From<SeatError> for Failure {
    fn from(error: SeatError) -> Self {
        Failure::Message(error.to_string())
    }
}

fn run(arguments: Vec<String>) -> Result<ExitCode, Failure> {
    // The computer-use command line is a thin client of an *already running*
    // runtime, so it is answered before any seat resolution: an Agent's shell
    // must not start or attach to a runtime to call one desktop tool. The
    // endpoint and its session token arrive in the environment the runtime
    // gave that Agent.
    if arguments
        .first()
        .is_some_and(|argument| argument == "computer")
    {
        return match vibex_computer::cli::run(&arguments[1..]) {
            Ok(output) => {
                println!("{output}");
                Ok(ExitCode::SUCCESS)
            }
            Err(error) => Err(Failure::Message(match &error.recovery_hint {
                Some(hint) => format!("{error}\n{hint}"),
                None => error.to_string(),
            })),
        };
    }
    if arguments.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Err(Failure::Usage(usage()));
    }
    if arguments
        .iter()
        .any(|arg| arg == "--version" || arg == "-V")
    {
        return Err(Failure::Usage(format!("vibex {VERSION}")));
    }

    let mut request = SeatRequest::default();
    let mut command = Command::Tui;
    let mut theme: Option<String> = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        match argument {
            "tui" => command = Command::Tui,
            "status" => command = Command::Status,
            "connect" => {
                let target = arguments.get(index + 1).cloned().ok_or_else(|| {
                    Failure::Message(
                        "`vibex connect` needs a vibex:// link or a pairing code".to_string(),
                    )
                })?;
                request.connect = Some(target);
                command = Command::Tui;
                index += 1;
            }
            "--home" => {
                let home = arguments
                    .get(index + 1)
                    .cloned()
                    .ok_or_else(|| Failure::Message("`--home` needs a directory".to_string()))?;
                request.home = Some(std::path::PathBuf::from(home));
                index += 1;
            }
            "--channel" => {
                let channel = arguments.get(index + 1).cloned().ok_or_else(|| {
                    Failure::Message("`--channel` needs stable, rc, or preview".to_string())
                })?;
                request.channel = Some(channel);
                index += 1;
            }
            "--local" => request.prefer_authority = true,
            "--remote" => request.prefer_authority = false,
            "--take-over" => request.take_over = true,
            "--theme" => {
                theme = arguments.get(index + 1).cloned();
                index += 1;
            }
            other if other.starts_with('-') => {
                return Err(Failure::Message(format!(
                    "unknown option `{other}`\n\n{}",
                    usage()
                )));
            }
            other => {
                // A bare argument is treated as a connection target, which is
                // what a pasted link produces.
                request.connect = Some(other.to_string());
            }
        }
        index += 1;
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| Failure::Message(format!("could not start the runtime: {error}")))?;

    // `status` answers "what would happen", so it probes instead of attaching.
    // Attaching first would make it fail for exactly the situations it exists
    // to explain.
    if command == Command::Status {
        let channel = vibex_client::seat::requested_channel(&request);
        let home =
            vibex_client::seat::resolve_home_for_channel(request.home.clone(), channel.as_deref())
                .map_err(Failure::from)?;
        let probe = vibex_client::seat::probe(&home);
        println!("home={}", probe.home.display());
        println!("flavour={}", probe.flavour.label());
        // Which channel chose that home: the packaged one, `--channel`, or
        // `VIBEX_CHANNEL`. `status` exists to explain a surprising home, and
        // "why this home" was the missing half of the answer.
        println!(
            "channel={}",
            vibex_client::seat::resolved_channel(channel.as_deref())
        );
        println!(
            "channel-source={}",
            if request.channel.is_some() {
                "flag"
            } else if channel.is_some() {
                "environment"
            } else {
                "packaged"
            }
        );
        println!(
            "seat={}",
            match probe.seat {
                SeatKind::Authority => "authority",
                SeatKind::Remote => "remote",
            }
        );
        if let Some(endpoint) = &probe.endpoint {
            println!("endpoint={endpoint}");
        }
        println!("detail={}", probe.detail);
        println!("version={VERSION}");
        return Ok(ExitCode::SUCCESS);
    }

    let seat = runtime
        .block_on(Seat::resolve(request))
        .map_err(Failure::from)?;

    // A client of somebody else's runtime can be offered the home if that
    // runtime goes away. The offer is watched here rather than inside the
    // interface: the lock is the host's business, and the interface is a client
    // that must not learn how to start a runtime.
    let handover = (seat.kind == SeatKind::Remote).then(vibex_tui::HandoverOffer::new);
    if let Some(offer) = handover.clone() {
        watch_home(&seat.home, offer);
    }

    let mut options = TuiOptions {
        seat: seat.kind,
        runtime_kind: seat.runtime_kind,
        handover,
        ..TuiOptions::default()
    };
    if let Some(theme) = theme {
        options.theme_id = Some(theme);
    }

    let result = vibex_tui::run(seat.facade.clone(), options);
    runtime.block_on(seat.shutdown());

    // The reader asked to own the home. This process is a client of the runtime
    // it has just left, so it restarts as one that owns it — the same binary,
    // the same home, one flag more.
    if matches!(result, Ok(ExitReason::Handover)) {
        return restart_as_authority(&arguments);
    }

    match result {
        Ok(ExitReason::UserQuit) => Ok(ExitCode::SUCCESS),
        // A lost connection is reported but is not a crash: the next run
        // reconnects.
        Ok(ExitReason::ConnectionLost) => {
            eprintln!("the connection to the runtime ended");
            Ok(ExitCode::SUCCESS)
        }
        // Answered above, before the runtime was shut down.
        Ok(ExitReason::Handover) => Ok(ExitCode::SUCCESS),
        Err(error) => Err(Failure::Message(format!(
            "{}: {}",
            error.code, error.message
        ))),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Tui,
    Status,
}

/// Publish whether the home this client is attached to has lost its runtime.
///
/// Polling is the honest shape here: the lock is a file, the runtime that held
/// it leaves no event behind, and a probe every few seconds costs one `flock`
/// that writes nothing. The offer is withdrawn as readily as it is made, so a
/// runtime that comes back takes the key away again.
fn watch_home(home: &std::path::Path, offer: vibex_tui::HandoverOffer) {
    let home = home.to_path_buf();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            offer.set_available(vibex_desktop_runtime::home_lock_is_free(&home));
        }
    });
}

/// Restart this client so it resolves its seat again, preferring the home.
///
/// `--local` is deliberately not used: the reader answered an offer that was
/// true when it was drawn, and a runtime that has started again since then must
/// leave them with a working client rather than a refusal.
fn restart_as_authority(arguments: &[String]) -> Result<ExitCode, Failure> {
    let executable = std::env::current_exe()
        .map_err(|error| Failure::Message(format!("could not find this executable: {error}")))?;
    let status = std::process::Command::new(executable)
        .args(arguments)
        .arg("--take-over")
        .status()
        .map_err(|error| Failure::Message(format!("could not restart the client: {error}")))?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn usage() -> String {
    let (channel, home) = match vibex_client::seat::resolved_channel(None) {
        "stable" => ("stable", "~/.vibex/desktop-stable"),
        "rc" => ("rc", "~/.vibex/desktop-rc"),
        _ => ("preview", "~/.vibex/desktop-preview"),
    };
    format!(
        "vibex {VERSION} — character-grid client for a Vibex runtime\n\
         \n\
         USAGE:\n\
         \x20   vibex [tui] [--home <dir>] [--channel <c>] [--local|--remote]\n\
         \x20          [--theme <id>]\n\
         \n\
         HOMES:\n\
         \x20   This client serves the {channel} channel it was packaged as, so\n\
         \x20   plain `vibex` opens {home}.\n\
         \x20   `--channel stable|rc|preview` (or VIBEX_CHANNEL) serves another\n\
         \x20   one. A home ending in desktop-preview / desktop-rc /\n\
         \x20   desktop-stable is that channel; anything else is a server home.\n\
         \x20   The flavour is taken from the path, because the runtime refuses to\n\
         \x20   start a channel in a home that does not match it.\n\
         \x20   vibex connect <vibex://… | pairing-code>\n\
         \x20   vibex status [--home <dir>] [--channel <c>]\n\
         \x20                                      report the mode without attaching\n\
         \x20   vibex computer <command>           drive the desktop; needs the\n\
         \x20                                     environment a running runtime\n\
         \x20                                     gives an Agent session\n\
         \n\
         MODES:\n\
         \x20   local mode   this process starts and owns the runtime for the home\n\
         \x20   remote mode  another runtime owns the home, or a link was given\n\
         \x20   `vibex status` prints the mode as seat=authority or seat=remote.\n\
         \n\
         ENVIRONMENT:\n\
         \x20   VIBEX_HOME        runtime home (default {home})\n\
         \x20   VIBEX_CHANNEL     stable | rc | preview; overrides the packaged one\n\
         \x20   VIBEX_THEME       theme id (default: appearance default)\n\
         \x20   VIBEX_TUI_COLOR   truecolor | ansi256 | 16 | none\n\
         \x20   VIBEX_TUI_ICONS   auto | emoji | ascii\n\
         \x20   VIBEX_TUI_KEYS    key-remap file (default <home>/tui-keys.toml)\n\
         \x20   VIBEX_TUI_LOG     spill file for process diagnostics while the\n\
         \x20                     interface owns the terminal (default\n\
         \x20                     $TMPDIR/vibex-tui-<pid>.log)\n\
         \x20   NO_COLOR          disable colour entirely\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_documents_every_entry_point() {
        let text = usage();
        for needle in [
            "vibex connect",
            "vibex status",
            "vibex computer",
            "--channel",
            "VIBEX_HOME",
            "VIBEX_CHANNEL",
            "local mode",
            "remote mode",
        ] {
            assert!(text.contains(needle), "usage is missing {needle}");
        }
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert!(matches!(
            run(vec!["--help".to_string()]),
            Err(Failure::Usage(_))
        ));
        match run(vec!["--version".to_string()]) {
            Err(Failure::Usage(text)) => assert_eq!(text, format!("vibex {VERSION}")),
            other => panic!("expected version output, got {other:?}"),
        }
    }

    #[test]
    fn status_reports_without_attaching() {
        // `status` must succeed where the real run would fail, because
        // explaining that failure is its whole purpose.
        let code = run(vec![
            "status".to_string(),
            "--home".to_string(),
            "/tmp/vibex-status-probe/desktop-preview".to_string(),
        ])
        .expect("status does not need a seat");
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn connect_without_a_target_is_rejected() {
        assert!(matches!(
            run(vec!["connect".to_string()]),
            Err(Failure::Message(_))
        ));
    }

    #[test]
    fn unknown_options_are_rejected_with_usage() {
        match run(vec!["--wat".to_string()]) {
            Err(Failure::Message(text)) => assert!(text.contains("USAGE")),
            other => panic!("expected a usage error, got {other:?}"),
        }
    }
}

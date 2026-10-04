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
            "--local" => request.prefer_authority = true,
            "--remote" => request.prefer_authority = false,
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
        let home = vibex_client::seat::resolve_home(request.home.clone()).map_err(Failure::from)?;
        let probe = vibex_client::seat::probe(&home);
        println!("home={}", probe.home.display());
        println!("flavour={}", probe.flavour.label());
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

    let mut options = TuiOptions {
        seat: seat.kind,
        ..TuiOptions::default()
    };
    if let Some(theme) = theme {
        options.theme_id = Some(theme);
    }

    let result = vibex_tui::run(seat.facade.clone(), options);
    runtime.block_on(seat.shutdown());

    match result {
        Ok(ExitReason::UserQuit) => Ok(ExitCode::SUCCESS),
        // A lost connection is reported but is not a crash: the next run
        // reconnects.
        Ok(ExitReason::ConnectionLost) => {
            eprintln!("the connection to the runtime ended");
            Ok(ExitCode::SUCCESS)
        }
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

fn usage() -> String {
    format!(
        "vibex {VERSION} — character-grid client for a Vibex runtime\n\
         \n\
         USAGE:\n\
         \x20   vibex [tui] [--home <dir>] [--local|--remote] [--theme <id>]\n\
         \n\
         HOMES:\n\
         \x20   A home ending in desktop-preview / desktop-rc / desktop-stable is\n\
         \x20   that desktop channel; anything else is a server home. The flavour is\n\
         \x20   taken from the path, because the runtime refuses to start a channel\n\
         \x20   in a home that does not match it.\n\
         \x20   vibex connect <vibex://… | pairing-code>\n\
         \x20   vibex status [--home <dir>]        report the mode without attaching\n\
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
         \x20   VIBEX_HOME        runtime home (default ~/.vibex/<channel>)\n\
         \x20   VIBEX_CHANNEL     stable | rc | preview\n\
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
            "VIBEX_HOME",
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

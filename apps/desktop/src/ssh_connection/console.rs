//! Console-window suppression for the remote-instance helpers.
//!
//! `ssh.exe`, `ssh-keygen.exe` and `wsl.exe` are console programs. The desktop
//! is a GUI-subsystem binary, so Windows gives every one of those children a
//! console of its own — a black "SSH" window that sits over the reader's
//! desktop for as long as the child runs. Piping the child's stdio does not
//! help: the console is allocated before the first byte is written.
//!
//! Every child the remote-instance code starts therefore goes through
//! [`hidden_command`], which passes `CREATE_NO_WINDOW`. A direct
//! `Command::new` in one of those files puts the window back, which is what
//! the module's test guards against.

use std::ffi::OsStr;
use std::process::Command;

/// `CREATE_NO_WINDOW` from `winbase.h`.
#[cfg(windows)]
pub(crate) const WINDOWS_CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A `Command` for a remote-instance helper that never gets a console window.
pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    #[cfg(windows)]
    let command = {
        use std::os::windows::process::CommandExt as _;

        let mut command = Command::new(program);
        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
        command
    };
    #[cfg(not(windows))]
    let command = Command::new(program);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The remote-instance helpers are exactly the files that spawn `ssh`,
    /// `ssh-keygen` or `wsl.exe`. A production `Command::new` in any of them is
    /// a console window the reader did not ask for, so this test fails instead
    /// of letting one back in.
    ///
    /// `SshCommand::new` and `WslCommand::new` are this module's own wrappers,
    /// not child spawns, so a match that continues a longer identifier does
    /// not count.
    #[test]
    fn remote_instance_children_never_use_command_new_directly() {
        fn direct_spawns(source: &str) -> Vec<&str> {
            let mut hits = Vec::new();
            let mut rest = source;
            while let Some(index) = rest.find("Command::new(") {
                let preceded_by_identifier = rest[..index]
                    .chars()
                    .next_back()
                    .is_some_and(|previous| previous.is_alphanumeric() || previous == '_');
                if !preceded_by_identifier {
                    let line_start = rest[..index].rfind('\n').map_or(0, |newline| newline + 1);
                    hits.push(&rest[line_start..index]);
                }
                rest = &rest[index + "Command::new(".len()..];
            }
            hits
        }

        for (name, source) in [
            ("command.rs", include_str!("command.rs")),
            ("config.rs", include_str!("config.rs")),
            ("host_key.rs", include_str!("host_key.rs")),
            ("tunnel.rs", include_str!("tunnel.rs")),
            ("wsl.rs", include_str!("wsl.rs")),
        ] {
            let production = source
                .split_once("\n#[cfg(test)]\nmod tests {")
                .map(|(production, _)| production)
                .unwrap_or(source);
            assert!(
                direct_spawns(production).is_empty(),
                "{name} starts a remote-instance child without `hidden_command` \
                 ({} occurrence(s)); Windows would show it in a console window",
                direct_spawns(production).len()
            );
        }
    }

    #[test]
    fn hidden_command_keeps_the_program_it_was_given() {
        let command = hidden_command("ssh");

        assert_eq!(command.get_program(), OsStr::new("ssh"));
    }
}

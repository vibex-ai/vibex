//! Command-line entry point for the installer's PATH helper.
//!
//! The installer calls this as
//! `vibex-installer-env add "$INSTDIR"` after copying files and
//! `vibex-installer-env remove "$INSTDIR"` from the uninstaller. It edits only
//! `HKCU\Environment\Path`; no other environment value is read or written.

#[cfg(windows)]
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("vibex-installer-env: {message}");
            ExitCode::from(code)
        }
    }
}

fn run() -> Result<(), (u8, String)> {
    let mut arguments = std::env::args().skip(1);
    let action = arguments.next().ok_or_else(|| (2, usage()))?;
    if matches!(action.as_str(), "-h" | "--help") {
        println!("{}", usage());
        return Ok(());
    }
    let directory = arguments.next().ok_or_else(|| (2, usage()))?;
    if arguments.next().is_some() {
        return Err((2, usage()));
    }
    if !matches!(action.as_str(), "add" | "remove") {
        return Err((2, format!("unknown action {action:?}\n{}", usage())));
    }

    #[cfg(not(windows))]
    {
        let _ = directory;
        Err((
            1,
            "this helper edits the Windows registry and cannot run here".to_owned(),
        ))
    }

    #[cfg(windows)]
    {
        validate_directory(&directory)?;
        let changed = match action.as_str() {
            "add" => vibex_installer_env::registry::add_directory(&directory),
            _ => vibex_installer_env::registry::remove_directory(&directory),
        }
        .map_err(|error| (1, error.to_string()))?;
        println!("{}", if changed { "changed" } else { "unchanged" });
        Ok(())
    }
}

#[cfg(windows)]
fn validate_directory(directory: &str) -> Result<(), (u8, String)> {
    let trimmed = directory.trim();
    if trimmed.is_empty() {
        return Err((2, "the directory is empty".to_owned()));
    }
    // A semicolon is the Path separator, so a directory containing one cannot
    // be represented as a single entry. Refusing is better than writing a path
    // that Windows will split into two entries.
    if directory.contains(';') {
        return Err((
            2,
            "the directory contains ';', the Path separator".to_owned(),
        ));
    }
    if directory.contains('\0') || directory.contains('"') {
        return Err((
            2,
            "the directory contains a character Path cannot hold".to_owned(),
        ));
    }
    // The installer passes $INSTDIR, which is absolute. A relative entry would
    // resolve against whatever directory a terminal happens to start in.
    if !Path::new(directory).is_absolute() {
        return Err((2, format!("the directory is not absolute: {directory}")));
    }
    Ok(())
}

fn usage() -> String {
    "usage: vibex-installer-env <add|remove> <absolute-directory>".to_owned()
}

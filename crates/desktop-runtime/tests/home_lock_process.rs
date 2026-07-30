use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

#[test]
fn second_shell_cannot_acquire_home_and_crash_releases_lock() {
    let home = tempfile::tempdir().unwrap();
    let executable = env!("CARGO_BIN_EXE_vibex-home-lock-probe");
    let mut first = Command::new(executable)
        .arg(home.path())
        .arg("30000")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = first.stdout.take().unwrap();
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "locked");

    let contending = Command::new(executable).arg(home.path()).output().unwrap();
    assert_eq!(contending.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&contending.stderr).contains("desktop_runtime_home_locked"));

    first.kill().unwrap();
    first.wait().unwrap();
    let recovered = Command::new(executable).arg(home.path()).output().unwrap();
    assert!(recovered.status.success());
    assert_eq!(String::from_utf8_lossy(&recovered.stdout).trim(), "locked");
}

//! Remote install of a downloaded server tarball.
//!
//! The tarball is uploaded over SSH stdin, then a fixed remote script verifies
//! its SHA-256, extracts the binary into a version directory, and swaps the
//! `current` symlink only after the new binary is in place. The old runtime is
//! left running and remains available through `previous`.

use std::fmt;
use std::fs;
#[cfg(test)]
use std::path::Path;
use std::time::Duration;

use super::artifact::LocalArtifact;
use super::command::SshCommand;
use super::error::SshFailure;

const MAX_REMOTE_ROOT_CHARS: usize = 1024;
const MAX_VERSION_CHARS: usize = 64;
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub binary_path: String,
    pub version: String,
    pub uploaded_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl InstallError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }

    fn retryable(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: true,
        }
    }
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for InstallError {}

/// Uploads `artifact` and installs it under `remote_root`.
pub fn install_artifact(
    command: &SshCommand,
    artifact: &LocalArtifact,
    remote_root: &str,
    progress: impl FnMut(u64, u64) + Send + 'static,
) -> Result<InstallOutcome, InstallError> {
    validate_remote_root(remote_root)?;
    validate_version(&artifact.version)?;
    if artifact.sha256.len() != 64 || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(InstallError::new(
            "ssh_install_digest_invalid",
            "the server artifact digest is invalid",
        ));
    }
    let bytes = fs::read(&artifact.path).map_err(|_| {
        InstallError::new(
            "ssh_install_artifact_read_failed",
            "the downloaded server artifact could not be read",
        )
    })?;
    if bytes.len() as u64 != artifact.size {
        return Err(InstallError::new(
            "ssh_install_artifact_size_mismatch",
            "the downloaded server artifact size changed before upload",
        ));
    }

    let incoming = incoming_path(remote_root, &artifact.version);
    let mut upload = command.clone();
    upload.args.extend([
        "sh".into(),
        "-c".into(),
        "set -eu; umask 077; cat > \"$1\"".into(),
        "sh".into(),
        incoming.clone().into(),
    ]);
    upload.stdin = Some(bytes);
    if let Err(error) = upload.run_with_progress(UPLOAD_TIMEOUT, progress) {
        cleanup_incoming(command, &incoming);
        return Err(map_ssh_failure(error));
    }

    let mut install = command.clone();
    install
        .remote_script(INSTALL_SCRIPT)
        .map_err(|error| InstallError::new("ssh_install_script_invalid", error.to_string()))?;
    install.args.extend([
        remote_root.into(),
        artifact.version.clone().into(),
        incoming.clone().into(),
        artifact.sha256.clone().into(),
    ]);
    if let Err(error) = install.run_with_timeout(INSTALL_TIMEOUT) {
        cleanup_incoming(command, &incoming);
        return Err(map_ssh_failure(error));
    }

    Ok(InstallOutcome {
        binary_path: format!("{remote_root}/{}/vibex-server", artifact.version),
        version: artifact.version.clone(),
        uploaded_bytes: artifact.size,
    })
}

/// The fixed remote install script. All values arrive as positional arguments;
/// none are interpolated into the script text.
pub const INSTALL_SCRIPT: &str = r#"set -eu
incoming=$1
root=$2
version=$3
expected=$4
actual=$(sha256sum "$incoming" | awk '{print $1}')
if [ "$actual" != "$expected" ]; then
  rm -f "$incoming"
  echo "install=hash_mismatch"
  exit 2
fi
listing=$(tar -tzf "$incoming")
if [ "$listing" != "vibex-server" ]; then
  rm -f "$incoming"
  echo "install=manifest_invalid"
  exit 3
fi
work="$root/.install-$$"
rm -rf "$work"
mkdir -p "$work"
tar -xzf "$incoming" -C "$work"
test -f "$work/vibex-server"
chmod 755 "$work/vibex-server"
mkdir -p "$root/$version"
mv -f "$work/vibex-server" "$root/$version/vibex-server"
rm -rf "$work" "$incoming"
old=""
if [ -L "$root/current" ]; then
  old=$(readlink "$root/current")
fi
if [ -n "$old" ]; then
  ln -sfn "$old" "$root/previous"
else
  rm -f "$root/previous"
fi
rm -f "$root/.current.tmp"
ln -s "$version" "$root/.current.tmp"
mv -Tf "$root/.current.tmp" "$root/current"
echo "install=ok version=$version"
"#;

fn cleanup_incoming(command: &SshCommand, incoming: &str) {
    let mut cleanup = command.clone();
    if cleanup.remote_script("rm -f \"$1\"").is_ok() {
        cleanup.args.push(incoming.into());
        let _ = cleanup.run_with_timeout(Duration::from_secs(15));
    }
}

fn incoming_path(remote_root: &str, version: &str) -> String {
    format!("{remote_root}/.incoming-{version}.tar.gz")
}

fn validate_remote_root(root: &str) -> Result<(), InstallError> {
    if root.is_empty()
        || root.len() > MAX_REMOTE_ROOT_CHARS
        || !root.starts_with('/')
        || root.contains('\0')
        || root.contains('\n')
        || root.contains('\r')
        || root.split('/').any(|part| part == "..")
    {
        return Err(InstallError::new(
            "ssh_install_root_invalid",
            "the remote install root is invalid",
        ));
    }
    Ok(())
}

fn validate_version(version: &str) -> Result<(), InstallError> {
    if version.is_empty()
        || version.chars().count() > MAX_VERSION_CHARS
        || !version.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
    {
        return Err(InstallError::new(
            "ssh_install_version_invalid",
            "the server version is not a safe directory name",
        ));
    }
    Ok(())
}

fn map_ssh_failure(failure: SshFailure) -> InstallError {
    if failure.is_retryable() {
        InstallError::retryable("ssh_install_failed", failure.message)
    } else {
        InstallError::new("ssh_install_failed", failure.message)
    }
}

/// Writes the install script to a file, for local script tests.
#[cfg(test)]
fn write_script(dir: &Path) -> std::path::PathBuf {
    let path = crate::ssh_connection::test_support::unique_path(dir, "install.sh");
    crate::ssh_connection::test_support::write_executable(&path, INSTALL_SCRIPT);
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_connection::command::SshDestination;
    use crate::ssh_connection::environment::RemoteArch;

    #[cfg(unix)]
    fn fake_ssh(dir: &Path, capture: &Path, fail_upload: bool) -> std::path::PathBuf {
        let path = crate::ssh_connection::test_support::unique_path(dir, "fake-ssh");
        let capture = capture.display();
        let fail = if fail_upload { "exit 255" } else { "exit 0" };
        crate::ssh_connection::test_support::write_executable(
            &path,
            &format!(
                "#!/bin/sh\n\
                 for arg in \"$@\"; do\n\
                   if [ \"$arg\" = \"-c\" ]; then\n\
                     cat > \"{capture}\"\n\
                     {fail}\n\
                   fi\n\
                   if [ \"$arg\" = \"-s\" ]; then\n\
                     cat >/dev/null\n\
                     echo 'install=ok version=test'\n\
                     exit 0\n\
                   fi\n\
                 done\n\
                 cat >/dev/null\n\
                 exit 0\n"
            ),
        );
        path
    }

    fn destination() -> SshDestination {
        SshDestination {
            host: "example.com".to_string(),
            user: Some("dev".to_string()),
            port: 22,
        }
    }

    fn local_artifact(dir: &Path, body: &[u8]) -> LocalArtifact {
        let path = dir.join("artifact.tar.gz");
        fs::write(&path, body).unwrap();
        LocalArtifact {
            path,
            version: "0.1.0-rc.8".to_string(),
            arch: RemoteArch::X86_64,
            size: body.len() as u64,
            sha256: hex_digest(body),
        }
    }

    fn hex_digest(bytes: &[u8]) -> String {
        use sha2::{Digest as _, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn script_is_fixed_and_swaps_current_only_after_extracting() {
        assert!(INSTALL_SCRIPT.contains("sha256sum"));
        assert!(INSTALL_SCRIPT.contains("tar -xzf"));
        assert!(INSTALL_SCRIPT.contains("mv -Tf"));
        assert!(INSTALL_SCRIPT.contains("previous"));
        assert!(INSTALL_SCRIPT.contains("install=ok"));
        assert!(!INSTALL_SCRIPT.contains("/home/"));
    }

    #[test]
    fn validates_remote_roots_and_versions() {
        assert!(validate_remote_root("/home/dev/.local/share/vibex/server").is_ok());
        for root in [
            "",
            "home/dev",
            "/home/../root",
            "/home/dev\n",
            "/home/dev\0",
        ] {
            assert!(validate_remote_root(root).is_err(), "{root:?}");
        }
        for version in ["", "../escape", "a/b", "a b", "x".repeat(65).as_str()] {
            assert!(validate_version(version).is_err(), "{version:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn uploads_the_exact_tarball_then_runs_the_install_script() {
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join("upload.bin");
        let ssh = fake_ssh(dir.path(), &capture, false);
        let command = SshCommand::for_destination(&ssh, &destination(), None).unwrap();
        let body = b"tarball bytes";
        let artifact = local_artifact(dir.path(), body);
        use std::sync::{Arc, Mutex};
        let progress = Arc::new(Mutex::new(Vec::new()));
        let progress_seen = progress.clone();
        let outcome = install_artifact(
            &command,
            &artifact,
            "/home/dev/.local/share/vibex/server",
            move |done, total| progress_seen.lock().unwrap().push((done, total)),
        )
        .unwrap_or_else(|error| panic!("install should succeed: {error:?}"));
        assert_eq!(
            fs::read(&capture)
                .unwrap_or_else(|error| panic!("upload capture should exist: {error:?}")),
            body
        );
        assert_eq!(outcome.version, "0.1.0-rc.8");
        assert!(
            progress
                .lock()
                .unwrap()
                .iter()
                .any(|(done, total)| *done == *total)
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_upload_failure_stops_before_install() {
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join("upload.bin");
        let ssh = fake_ssh(dir.path(), &capture, true);
        let command = SshCommand::for_destination(&ssh, &destination(), None).unwrap();
        let artifact = local_artifact(dir.path(), b"tarball bytes");
        let error = install_artifact(
            &command,
            &artifact,
            "/home/dev/.local/share/vibex/server",
            |_, _| {},
        )
        .unwrap_err();
        assert_eq!(error.code, "ssh_install_failed");
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn the_install_script_extracts_swaps_current_and_records_previous() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::process::Command;

        let _process_guard = crate::ssh_connection::test_support::process_guard();

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("server");
        fs::create_dir_all(&root).unwrap();
        let stage = dir.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let old_binary = stage.join("vibex-server");
        fs::write(&old_binary, "#!/bin/sh\necho old\n").unwrap();
        fs::set_permissions(&old_binary, fs::Permissions::from_mode(0o755)).unwrap();
        let old_tar = dir.path().join("old.tar.gz");
        let status = Command::new("tar")
            .args([
                "-czf",
                old_tar.to_str().unwrap(),
                "-C",
                stage.to_str().unwrap(),
                "vibex-server",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let old_sha = hex_digest(&fs::read(&old_tar).unwrap());

        let run = |incoming: &Path, version: &str, sha: &str| {
            let script = write_script(dir.path());
            let child = Command::new("sh")
                .arg(script)
                .args([
                    incoming.to_str().unwrap(),
                    root.to_str().unwrap(),
                    version,
                    sha,
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child.wait_with_output().unwrap()
        };

        let first = run(&old_tar, "0.1.0", &old_sha);
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        assert!(root.join("current").is_symlink());
        assert_eq!(
            fs::read_link(root.join("current")).unwrap(),
            std::path::PathBuf::from("0.1.0")
        );
        assert!(!root.join("previous").exists());

        fs::write(&old_binary, "#!/bin/sh\necho new\n").unwrap();
        let new_tar = dir.path().join("new.tar.gz");
        let status = Command::new("tar")
            .args([
                "-czf",
                new_tar.to_str().unwrap(),
                "-C",
                stage.to_str().unwrap(),
                "vibex-server",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let new_sha = hex_digest(&fs::read(&new_tar).unwrap());
        let second = run(&new_tar, "0.2.0", &new_sha);
        assert!(
            second.status.success(),
            "{}",
            String::from_utf8_lossy(&second.stderr)
        );
        assert_eq!(
            fs::read_link(root.join("current")).unwrap(),
            std::path::PathBuf::from("0.2.0")
        );
        assert_eq!(
            fs::read_link(root.join("previous")).unwrap(),
            std::path::PathBuf::from("0.1.0")
        );
        assert!(root.join("0.1.0/vibex-server").is_file());
        assert!(root.join("0.2.0/vibex-server").is_file());
    }
}

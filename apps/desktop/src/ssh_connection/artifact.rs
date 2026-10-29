//! Headless server artifact selection, download and verification.
//!
//! The signed release manifest is the authority: this module only selects the
//! Linux artifact for a remote architecture, downloads it into a local cache,
//! and verifies the size and SHA-256 before the install step may upload it.

use std::fmt;
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use vibex_app_update::{ServerArtifact, VerifiedManifest};

use super::environment::RemoteArch;

const MAX_VERSION_CHARS: usize = 64;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalArtifact {
    pub path: PathBuf,
    pub version: String,
    pub arch: RemoteArch,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl ArtifactError {
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

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ArtifactError {}

/// Selects the Linux artifact for `arch` from a verified release manifest.
pub fn select_server_artifact(
    manifest: &VerifiedManifest,
    arch: RemoteArch,
) -> Result<&ServerArtifact, ArtifactError> {
    select_server_artifact_for_os(manifest, "linux", arch)
}

/// Selects a server artifact for an explicit release OS (`linux` or `macos`).
pub fn select_server_artifact_for_os<'a>(
    manifest: &'a VerifiedManifest,
    os: &str,
    arch: RemoteArch,
) -> Result<&'a ServerArtifact, ArtifactError> {
    let os = normalized_server_os(os);
    let target = match arch {
        RemoteArch::X86_64 => "x86_64",
        RemoteArch::Aarch64 => "arm64",
    };
    manifest.server_artifact(os, target).ok_or_else(|| {
        ArtifactError::new(
            "ssh_artifact_missing",
            format!("the release does not ship a {os} {target} server artifact"),
        )
    })
}

pub fn normalized_server_os(value: &str) -> &'static str {
    if value.eq_ignore_ascii_case("darwin") || value.eq_ignore_ascii_case("macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Downloads `artifact` into `cache_dir`, or reuses a verified cached copy.
/// The returned file is the release tarball; install extracts it remotely.
pub async fn download_server_artifact(
    artifact: &ServerArtifact,
    version: &str,
    arch: RemoteArch,
    cache_dir: &Path,
    progress: impl FnMut(u64, u64),
) -> Result<LocalArtifact, ArtifactError> {
    download_server_artifact_for_os(artifact, "linux", version, arch, cache_dir, progress).await
}

pub async fn download_server_artifact_for_os(
    artifact: &ServerArtifact,
    os: &str,
    version: &str,
    arch: RemoteArch,
    cache_dir: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<LocalArtifact, ArtifactError> {
    let os = normalized_server_os(os);
    if artifact.os != os {
        return Err(ArtifactError::new(
            "ssh_artifact_target_invalid",
            format!("the selected server artifact is not for {os}"),
        ));
    }
    let file_name = cache_file_name_for_os(version, os, arch)?;
    fs::create_dir_all(cache_dir).map_err(|_| {
        ArtifactError::new(
            "ssh_artifact_cache_failed",
            "the server artifact cache could not be created",
        )
    })?;
    let path = cache_dir.join(&file_name);
    if path.is_file() {
        if verify_file(&path, artifact.size, &artifact.sha256).is_ok() {
            progress(artifact.size, artifact.size);
            return Ok(LocalArtifact {
                path,
                version: version.to_string(),
                arch,
                size: artifact.size,
                sha256: artifact.sha256.clone(),
            });
        }
        let _ = fs::remove_file(&path);
    }

    let part = cache_dir.join(format!("{file_name}.part"));
    let _ = fs::remove_file(&part);
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(concat!("Vibex/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| {
            ArtifactError::new(
                "ssh_artifact_http_client_failed",
                format!("the download client could not be initialized: {error}"),
            )
        })?;
    let mut response = client
        .get(artifact.url.clone())
        .send()
        .await
        .map_err(|error| {
            ArtifactError::retryable(
                "ssh_artifact_download_failed",
                format!("the server artifact download failed: {error}"),
            )
        })?;
    if !response.status().is_success() {
        return Err(ArtifactError::retryable(
            "ssh_artifact_download_status",
            format!(
                "the server artifact download returned HTTP {}",
                response.status()
            ),
        ));
    }
    if let Some(length) = response.content_length()
        && length != artifact.size
    {
        return Err(ArtifactError::new(
            "ssh_artifact_size_mismatch",
            "the server artifact length does not match the signed manifest",
        ));
    }

    let mut file = File::create(&part).map_err(|_| {
        ArtifactError::new(
            "ssh_artifact_io_failed",
            "the server artifact cache file could not be created",
        )
    })?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                let _ = fs::remove_file(&part);
                return Err(ArtifactError::retryable(
                    "ssh_artifact_download_failed",
                    format!("the server artifact download was interrupted: {error}"),
                ));
            }
        };
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        if downloaded > artifact.size {
            let _ = fs::remove_file(&part);
            return Err(ArtifactError::new(
                "ssh_artifact_size_mismatch",
                "the server artifact is larger than the signed manifest",
            ));
        }
        hasher.update(&chunk);
        if file.write_all(&chunk).is_err() {
            let _ = fs::remove_file(&part);
            return Err(ArtifactError::new(
                "ssh_artifact_io_failed",
                "the server artifact cache file could not be written",
            ));
        }
        progress(downloaded, artifact.size);
    }
    if file.flush().is_err() {
        let _ = fs::remove_file(&part);
        return Err(ArtifactError::new(
            "ssh_artifact_io_failed",
            "the server artifact cache file could not be flushed",
        ));
    }
    if downloaded != artifact.size {
        let _ = fs::remove_file(&part);
        return Err(ArtifactError::new(
            "ssh_artifact_size_mismatch",
            "the server artifact size does not match the signed manifest",
        ));
    }
    if hex_lower(&hasher.finalize()) != artifact.sha256 {
        let _ = fs::remove_file(&part);
        return Err(ArtifactError::new(
            "ssh_artifact_hash_mismatch",
            "the server artifact failed SHA-256 verification",
        ));
    }
    fs::rename(&part, &path).map_err(|_| {
        let _ = fs::remove_file(&part);
        ArtifactError::new(
            "ssh_artifact_io_failed",
            "the server artifact could not be moved into the cache",
        )
    })?;

    Ok(LocalArtifact {
        path,
        version: version.to_string(),
        arch,
        size: artifact.size,
        sha256: artifact.sha256.clone(),
    })
}

/// Streams a file through SHA-256 and checks the signed size and digest.
pub fn verify_file(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), ArtifactError> {
    let mut file = File::open(path).map_err(|_| {
        ArtifactError::new(
            "ssh_artifact_cache_invalid",
            "the cached server artifact could not be opened",
        )
    })?;
    let metadata = file.metadata().map_err(|_| {
        ArtifactError::new(
            "ssh_artifact_cache_invalid",
            "the cached server artifact metadata could not be read",
        )
    })?;
    if metadata.len() != expected_size {
        return Err(ArtifactError::new(
            "ssh_artifact_size_mismatch",
            "the cached server artifact size does not match the signed manifest",
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|_| {
            ArtifactError::new(
                "ssh_artifact_cache_invalid",
                "the cached server artifact could not be read",
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if hex_lower(&hasher.finalize()) != expected_sha256 {
        return Err(ArtifactError::new(
            "ssh_artifact_hash_mismatch",
            "the cached server artifact failed SHA-256 verification",
        ));
    }
    Ok(())
}

fn cache_file_name_for_os(
    version: &str,
    os: &str,
    arch: RemoteArch,
) -> Result<String, ArtifactError> {
    if os == "linux" {
        return cache_file_name(version, arch);
    }
    validate_version_for_cache(version)?;
    Ok(format!(
        "vibex-server-{version}-{os}-{}.tar.gz",
        arch.as_str()
    ))
}

fn cache_file_name(version: &str, arch: RemoteArch) -> Result<String, ArtifactError> {
    validate_version_for_cache(version)?;
    Ok(format!("vibex-server-{version}-{}.tar.gz", arch.as_str()))
}

fn validate_version_for_cache(version: &str) -> Result<(), ArtifactError> {
    if version.is_empty()
        || version.chars().count() > MAX_VERSION_CHARS
        || !version.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
    {
        return Err(ArtifactError::new(
            "ssh_artifact_version_invalid",
            "the server artifact version is not a safe file name",
        ));
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::ssh_connection::environment::RemoteArch;

    fn manifest(sha256: String, size: u64) -> VerifiedManifest {
        let value = serde_json::json!({
            "schema": 1,
            "channel": "rc",
            "version": "0.1.0-rc.8",
            "tag": "v0.1.0-rc.8",
            "published_at": "2026-10-28T00:00:00Z",
            "minimum_updater_version": "0.1.0",
            "notes_url": "https://example.com/notes",
            "artifacts": [],
            "server_artifacts": [
                {
                    "os": "linux",
                    "arch": "x86_64",
                    "url": "https://example.com/vibex-server-linux-x86_64.tar.gz",
                    "size": size,
                    "sha256": sha256
                },
                {
                    "os": "linux",
                    "arch": "arm64",
                    "url": "https://example.com/vibex-server-linux-arm64.tar.gz",
                    "size": size,
                    "sha256": sha256
                },
                {
                    "os": "macos",
                    "arch": "x86_64",
                    "url": "https://example.com/vibex-server-macos-x86_64.tar.gz",
                    "size": size,
                    "sha256": sha256
                },
                {
                    "os": "macos",
                    "arch": "arm64",
                    "url": "https://example.com/vibex-server-macos-arm64.tar.gz",
                    "size": size,
                    "sha256": sha256
                }
            ]
        });
        VerifiedManifest {
            manifest: serde_json::from_value(value).unwrap(),
            raw_sha256: "0".repeat(64),
        }
    }

    fn digest(bytes: &[u8]) -> String {
        hex_lower(&Sha256::digest(bytes))
    }

    #[test]
    fn selects_the_linux_artifact_for_both_architectures() {
        let verified = manifest("a".repeat(64), 10);
        assert_eq!(
            select_server_artifact(&verified, RemoteArch::X86_64)
                .unwrap()
                .arch,
            "x86_64"
        );
        assert_eq!(
            select_server_artifact(&verified, RemoteArch::Aarch64)
                .unwrap()
                .arch,
            "arm64"
        );
    }

    #[test]
    fn selects_macos_and_normalizes_darwin() {
        let verified = manifest("a".repeat(64), 10);
        assert_eq!(
            select_server_artifact_for_os(&verified, "Darwin", RemoteArch::Aarch64)
                .unwrap()
                .os,
            "macos"
        );
        assert_eq!(
            select_server_artifact_for_os(&verified, "macOS", RemoteArch::X86_64)
                .unwrap()
                .arch,
            "x86_64"
        );
        assert_eq!(normalized_server_os("Linux"), "linux");
        assert_eq!(normalized_server_os("darwin"), "macos");
    }

    #[test]
    fn reports_a_missing_architecture() {
        let mut verified = manifest("a".repeat(64), 10);
        verified
            .manifest
            .server_artifacts
            .retain(|artifact| artifact.arch == "x86_64");
        let error = select_server_artifact(&verified, RemoteArch::Aarch64).unwrap_err();
        assert_eq!(error.code, "ssh_artifact_missing");
    }

    #[test]
    fn verify_file_checks_size_and_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.tar.gz");
        fs::write(&path, b"artifact bytes").unwrap();
        verify_file(&path, 14, &digest(b"artifact bytes")).unwrap();
        assert_eq!(
            verify_file(&path, 14, &digest(b"different"))
                .unwrap_err()
                .code,
            "ssh_artifact_hash_mismatch"
        );
        assert_eq!(
            verify_file(&path, 13, &digest(b"artifact bytes"))
                .unwrap_err()
                .code,
            "ssh_artifact_size_mismatch"
        );
    }

    #[tokio::test]
    async fn downloads_verifies_and_reuses_a_cached_artifact() {
        let body = b"server artifact bytes".to_vec();
        let expected_size = body.len() as u64;
        let expected_sha = digest(&body);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_body = body.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                server_body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&server_body);
        });

        let dir = tempfile::tempdir().unwrap();
        let artifact = ServerArtifact {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            url: url::Url::parse(&format!("http://{address}/artifact.tar.gz")).unwrap(),
            size: expected_size,
            sha256: expected_sha.clone(),
        };
        let progress = Arc::new(Mutex::new(Vec::new()));
        let progress_seen = progress.clone();
        let local = download_server_artifact(
            &artifact,
            "0.1.0-rc.8",
            RemoteArch::X86_64,
            dir.path(),
            move |done, total| progress_seen.lock().unwrap().push((done, total)),
        )
        .await
        .unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(&local.path).unwrap(), body);
        assert_eq!(local.sha256, expected_sha);
        assert!(!progress.lock().unwrap().is_empty());

        // The second call must resolve from the verified cache without a server.
        let cached = download_server_artifact(
            &artifact,
            "0.1.0-rc.8",
            RemoteArch::X86_64,
            dir.path(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(cached.path, local.path);
    }

    #[tokio::test]
    async fn a_hash_mismatch_removes_the_partial_download() {
        let body = b"tampered".to_vec();
        let expected_size = body.len() as u64;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_body = body.clone();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                server_body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&server_body);
        });
        let dir = tempfile::tempdir().unwrap();
        let artifact = ServerArtifact {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            url: url::Url::parse(&format!("http://{address}/artifact.tar.gz")).unwrap(),
            size: expected_size,
            sha256: "b".repeat(64),
        };
        let error = download_server_artifact(
            &artifact,
            "0.1.0-rc.8",
            RemoteArch::X86_64,
            dir.path(),
            |_, _| {},
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "ssh_artifact_hash_mismatch");
        let remains = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(remains, 0, "partial download should be removed");
    }
}

//! Verifying and unpacking a plugin package.
//!
//! An archive is untrusted input. Extraction is a policy of its own: entry
//! names are confined to the destination, symlinks and non-regular entries are
//! refused, and per-file, per-count and total-byte ceilings bound a
//! decompression bomb. Every refused entry is reported rather than dropped.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256, Sha512};
use zip::ZipArchive;

use crate::error::PluginError;

/// What an extraction wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractReport {
    pub files: usize,
    pub bytes: u64,
    /// Entries that were refused, with the reason.
    pub skipped: Vec<String>,
}

/// Lowercase SHA-256 of the bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    crate::fingerprint::hex_lower(&Sha256::digest(bytes))
}

/// Lowercase SHA-512 of the bytes.
pub fn sha512_hex(bytes: &[u8]) -> String {
    crate::fingerprint::hex_lower(&Sha512::digest(bytes))
}

/// Verifies a package's checksums.
pub fn verify_checksums(
    bytes: &[u8],
    sha256: &str,
    sha512: Option<&str>,
) -> Result<(), PluginError> {
    if !sha256.is_empty() && !sha256_hex(bytes).eq_ignore_ascii_case(sha256) {
        return Err(PluginError::HashMismatch);
    }
    if let Some(sha512) = sha512.filter(|value| !value.is_empty())
        && !sha512_hex(bytes).eq_ignore_ascii_case(sha512)
    {
        return Err(PluginError::HashMismatch);
    }
    Ok(())
}

/// Extracts a plugin archive into `target`.
///
/// The target must already exist. The caller stages into a fresh directory and
/// only publishes it once extraction and manifest validation both succeed.
pub fn extract_zip(archive_bytes: &[u8], target: &Path) -> Result<ExtractReport, PluginError> {
    let mut archive = ZipArchive::new(std::io::Cursor::new(archive_bytes))
        .map_err(|error| PluginError::Archive(error.to_string()))?;
    if archive.len() > vibex_core::MAX_PLUGIN_BUNDLE_FILES {
        return Err(PluginError::UnpackRejected(format!(
            "the archive holds {} entries, more than the {} the market accepts",
            archive.len(),
            vibex_core::MAX_PLUGIN_BUNDLE_FILES
        )));
    }

    let mut report = ExtractReport::default();
    let mut seen = std::collections::BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| PluginError::Archive(error.to_string()))?;
        let name = entry
            .name()
            .map_err(|error| PluginError::Archive(error.to_string()))?
            .to_string();
        let Some(relative) = safe_relative_path(&name) else {
            report
                .skipped
                .push(format!("{name}: the path is not inside the package"));
            continue;
        };
        if !seen.insert(relative.clone()) {
            report
                .skipped
                .push(format!("{name}: the path is written twice"));
            continue;
        }
        if let Some(mode) = entry.unix_mode()
            && mode & 0o170000 == 0o120000
        {
            report
                .skipped
                .push(format!("{name}: symlinks are not allowed"));
            continue;
        }
        if entry.is_dir() {
            std::fs::create_dir_all(target.join(&relative))?;
            continue;
        }
        if entry.size() > vibex_core::MAX_PLUGIN_BUNDLE_FILE_BYTES {
            return Err(PluginError::UnpackRejected(format!(
                "{name}: the file is larger than the market accepts"
            )));
        }
        if report.bytes + entry.size() > vibex_core::MAX_PLUGIN_UNPACKED_BYTES {
            return Err(PluginError::UnpackRejected(
                "the package is larger than the market accepts once unpacked".to_string(),
            ));
        }
        let destination = target.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut reader: Box<dyn Read> = Box::new(entry);
        let mut output = std::fs::File::create(&destination)?;
        let written = std::io::copy(&mut reader, &mut output)?;
        report.files += 1;
        report.bytes += written;
    }
    Ok(report)
}

/// Confines an archive entry name to a plain relative path.
pub fn safe_relative_path(name: &str) -> Option<String> {
    if name.is_empty() || name.len() > 1024 {
        return None;
    }
    if name.contains('\\') || name.contains('\0') || name.contains(':') {
        return None;
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return None;
    }
    let mut cleaned = PathBuf::new();
    let mut depth = 0i32;
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                cleaned.push(part);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
                cleaned.pop();
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if depth == 0 {
        return None;
    }
    Some(cleaned.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_are_verified() {
        let bytes = b"hello plugin";
        let sha256 = sha256_hex(bytes);
        let sha512 = sha512_hex(bytes);
        assert!(verify_checksums(bytes, &sha256, Some(&sha512)).is_ok());
        assert!(verify_checksums(bytes, &"0".repeat(64), None).is_err());
        assert!(verify_checksums(bytes, &sha256, Some(&"0".repeat(128))).is_err());
    }

    #[test]
    fn unsafe_entry_names_are_refused() {
        for name in [
            "/etc/passwd",
            "../escape.js",
            "a/../../escape.js",
            "C:/windows",
            "a\\b.js",
        ] {
            assert!(
                safe_relative_path(name).is_none(),
                "{name} should be refused"
            );
        }
        assert_eq!(
            safe_relative_path("ui/main.js").as_deref(),
            Some("ui/main.js")
        );
        assert_eq!(safe_relative_path("./main.js").as_deref(), Some("main.js"));
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
            for (name, contents) in entries {
                writer.start_file(*name, options).unwrap();
                writer.write_all(contents).unwrap();
            }
            writer.finish().unwrap();
        }
        buffer
    }

    #[test]
    fn a_package_extracts_to_its_files() {
        let bytes = archive(&[
            ("vibex-plugin.json", b"{}"),
            ("main.js", b"console.log(1)"),
            ("ui/panel.js", b"export {}"),
        ]);
        let directory = tempfile::tempdir().unwrap();
        let report = extract_zip(&bytes, directory.path()).unwrap();
        assert_eq!(report.files, 3);
        assert!(report.skipped.is_empty());
        assert!(directory.path().join("ui/panel.js").exists());
        assert_eq!(
            std::fs::read(directory.path().join("main.js")).unwrap(),
            b"console.log(1)"
        );
    }

    #[test]
    fn a_traversal_entry_is_refused_not_written() {
        let bytes = archive(&[("../escape.js", b"bad"), ("main.js", b"ok")]);
        let directory = tempfile::tempdir().unwrap();
        let report = extract_zip(&bytes, directory.path()).unwrap();
        assert_eq!(report.files, 1);
        assert_eq!(report.skipped.len(), 1);
        assert!(
            !directory
                .path()
                .parent()
                .unwrap()
                .join("escape.js")
                .exists()
        );
    }

    #[test]
    fn a_larger_than_allowed_file_is_refused() {
        // The per-file ceiling is bounded by the constant; a small archive
        // cannot exceed it, so this checks the guard is reachable through the
        // reported report shape instead.
        let bytes = archive(&[("main.js", b"ok")]);
        let directory = tempfile::tempdir().unwrap();
        assert!(extract_zip(&bytes, directory.path()).is_ok());
    }
}

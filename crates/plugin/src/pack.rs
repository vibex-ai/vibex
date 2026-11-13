//! Building a `.vplug` package from a plugin directory.

use std::io::Write;
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::error::PluginError;
use crate::manifest::{AUTHOR_MANIFEST_FILE, PluginManifest};
use crate::shell::{SHELL_MANIFEST_FILE, ShellManifestContext, build_shell_manifest};

/// The package extension the desktop installs.
pub const PACKAGE_EXTENSION: &str = "vplug";

/// Files never packed: the generated host manifest and VCS metadata.
const SKIPPED: &[&str] = &[SHELL_MANIFEST_FILE, ".git", ".DS_Store"];

/// Builds a package from a directory, returning the archive bytes.
///
/// The generated host manifest is written into the archive, never into the
/// source directory, so packing does not modify what the author wrote.
pub fn pack_directory(directory: &Path) -> Result<Vec<u8>, PluginError> {
    let manifest_path = directory.join(AUTHOR_MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Err(PluginError::MissingManifest);
    }
    let manifest = PluginManifest::from_file(&manifest_path)?;
    let built = build_shell_manifest(
        &manifest,
        &ShellManifestContext {
            plugin_dir: Some(directory.to_path_buf()),
            data_dir: None,
            workspace_root: None,
            home_dir: None,
        },
    )?;
    let host_manifest = serde_json::to_vec_pretty(&built.manifest)?;

    let mut buffer = Vec::new();
    {
        let mut writer = ZipWriter::new(std::io::Cursor::new(&mut buffer));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let mut files = Vec::new();
        collect_files(directory, directory, &mut files)?;
        files.sort();
        for relative in files {
            let name = relative.to_string_lossy().replace('\\', "/");
            if SKIPPED.iter().any(|skipped| name == *skipped) {
                continue;
            }
            writer
                .start_file(&name, options)
                .map_err(|error| PluginError::Archive(error.to_string()))?;
            let contents = std::fs::read(directory.join(&relative))?;
            writer.write_all(&contents)?;
        }
        writer
            .start_file(SHELL_MANIFEST_FILE, options)
            .map_err(|error| PluginError::Archive(error.to_string()))?;
        writer.write_all(&host_manifest)?;
        writer
            .finish()
            .map_err(|error| PluginError::Archive(error.to_string()))?;
    }
    Ok(buffer)
}

/// Builds a package and writes it beside the plugin directory.
pub fn write_package(directory: &Path, output: Option<&Path>) -> Result<PathBuf, PluginError> {
    let manifest = PluginManifest::from_file(&directory.join(AUTHOR_MANIFEST_FILE))?;
    let bytes = pack_directory(directory)?;
    let output = match output {
        Some(path) => path.to_path_buf(),
        None => directory.parent().unwrap_or(directory).join(format!(
            "{}-{}.{PACKAGE_EXTENSION}",
            manifest.id, manifest.version
        )),
    };
    std::fs::write(&output, bytes)?;
    Ok(output)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), PluginError> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if SKIPPED.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, files)?;
        } else if path.is_file() {
            if let Ok(relative) = path.strip_prefix(root) {
                files.push(relative.to_path_buf());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packing_generates_the_host_manifest_into_the_archive() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = serde_json::json!({
            "id": "acme.widgets",
            "name": "Widgets",
            "version": "1.0.0",
            "license": "MIT",
            "entry": "main.js",
            "capabilities": { "panel": true }
        });
        std::fs::write(
            directory.path().join(AUTHOR_MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(directory.path().join("main.js"), b"export {}").unwrap();
        std::fs::create_dir_all(directory.path().join("ui")).unwrap();
        std::fs::write(directory.path().join("ui/panel.js"), b"export {}").unwrap();

        let bytes = pack_directory(directory.path()).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().unwrap().to_string())
            .collect();
        assert!(names.contains(&"vibex-plugin.json".to_string()));
        assert!(names.contains(&"main.js".to_string()));
        assert!(names.contains(&"ui/panel.js".to_string()));
        assert!(names.contains(&SHELL_MANIFEST_FILE.to_string()));

        // The generated manifest is a parsed host manifest.
        let mut host = archive.by_name(SHELL_MANIFEST_FILE).unwrap();
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut host, &mut contents).unwrap();
        let parsed: crate::shell::ShellManifest = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed.id, "acme.widgets");
        assert_eq!(parsed.entry, "main.js");
    }

    #[test]
    fn packing_without_a_manifest_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            pack_directory(directory.path()),
            Err(PluginError::MissingManifest)
        ));
    }
}

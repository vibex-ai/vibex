use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

use crate::{AppUpdateError, AppUpdateResult, InstallMode, UpdateArtifact};

const PENDING_INSTALL_FILE: &str = "pending-install.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationKind {
    AppImage,
    Deb,
    Rpm,
    Flatpak,
    MacApp,
    MacAppStore,
    WindowsInstaller,
    WindowsStore,
    Unmanaged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub kind: InstallationKind,
    pub package: String,
    pub target_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallOutcome {
    RestartRequired,
    InstallerLaunched,
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingInstall {
    version: String,
    target: PathBuf,
    backup: PathBuf,
}

impl Installation {
    pub fn detect() -> Self {
        if let Some(package) = std::env::var_os("VIBEX_INSTALL_PACKAGE") {
            return Self::from_package_override(package.to_string_lossy().as_ref());
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(path) = std::env::var_os("APPIMAGE") {
                return Self {
                    kind: InstallationKind::AppImage,
                    package: "appimage".to_string(),
                    target_path: Some(PathBuf::from(path)),
                };
            }
            if std::env::var_os("FLATPAK_ID").is_some() {
                return Self {
                    kind: InstallationKind::Flatpak,
                    package: "flatpak".to_string(),
                    target_path: None,
                };
            }
            let executable = std::env::current_exe().ok();
            if executable
                .as_deref()
                .is_some_and(|path| path.starts_with("/usr"))
            {
                let (kind, package) = if Path::new("/etc/debian_version").exists() {
                    (InstallationKind::Deb, "deb")
                } else {
                    (InstallationKind::Rpm, "rpm")
                };
                return Self {
                    kind,
                    package: package.to_string(),
                    target_path: executable,
                };
            }
        }
        #[cfg(target_os = "macos")]
        {
            let executable = std::env::current_exe().ok();
            let app_root = executable.as_deref().and_then(mac_app_root);
            let store = app_root
                .as_deref()
                .is_some_and(|root| root.join("Contents/_MASReceipt/receipt").exists());
            return Self {
                kind: if store {
                    InstallationKind::MacAppStore
                } else {
                    InstallationKind::MacApp
                },
                package: if store { "mac_app_store" } else { "app" }.to_string(),
                target_path: app_root,
            };
        }
        #[cfg(target_os = "windows")]
        {
            let store = std::env::var_os("APPX_PACKAGE_FAMILY_NAME").is_some();
            return Self {
                kind: if store {
                    InstallationKind::WindowsStore
                } else {
                    InstallationKind::WindowsInstaller
                },
                package: if store { "windows_store" } else { "nsis" }.to_string(),
                target_path: std::env::current_exe().ok(),
            };
        }
        Self {
            kind: InstallationKind::Unmanaged,
            package: "unmanaged".to_string(),
            target_path: std::env::current_exe().ok(),
        }
    }

    pub fn supports(&self, install_mode: InstallMode) -> bool {
        match install_mode {
            InstallMode::SelfReplace => self.kind == InstallationKind::AppImage,
            InstallMode::SystemInstaller => matches!(
                self.kind,
                InstallationKind::Deb
                    | InstallationKind::Rpm
                    | InstallationKind::MacApp
                    | InstallationKind::WindowsInstaller
            ),
            InstallMode::Store | InstallMode::External => false,
        }
    }

    pub(crate) fn install(
        &self,
        artifact: &UpdateArtifact,
        staged_path: &Path,
        version: &str,
        updates_dir: &Path,
    ) -> AppUpdateResult<InstallOutcome> {
        if !self.supports(artifact.install_mode) {
            return Err(AppUpdateError::new(
                "app_update_install_mode_unsupported",
                "this installation is managed by an external package source",
            ));
        }
        match artifact.install_mode {
            InstallMode::SelfReplace => {
                let target = self.target_path.as_deref().ok_or_else(|| {
                    AppUpdateError::new(
                        "app_update_install_target_missing",
                        "the current application package could not be located",
                    )
                })?;
                replace_appimage(target, staged_path, version, updates_dir)?;
                Ok(InstallOutcome::RestartRequired)
            }
            InstallMode::SystemInstaller => {
                launch_system_installer(staged_path)?;
                Ok(InstallOutcome::InstallerLaunched)
            }
            InstallMode::Store | InstallMode::External => unreachable!("unsupported above"),
        }
    }

    pub(crate) fn restart(&self) -> AppUpdateResult<()> {
        let target = self.target_path.as_deref().ok_or_else(|| {
            AppUpdateError::new(
                "app_update_restart_target_missing",
                "the updated application could not be located",
            )
        })?;
        #[cfg(unix)]
        let result = Command::new("sh")
            .arg("-c")
            .arg("while kill -0 \"$1\" 2>/dev/null; do sleep 0.1; done; exec \"$2\"")
            .arg("vibex-update-restart")
            .arg(std::process::id().to_string())
            .arg(target)
            .spawn();
        #[cfg(target_os = "windows")]
        let result = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Wait-Process -Id $args[0]; Start-Process -FilePath $args[1]",
            ])
            .arg(std::process::id().to_string())
            .arg(target)
            .spawn();
        #[cfg(not(any(unix, target_os = "windows")))]
        let result = Command::new(target).spawn();
        result.map(|_| ()).map_err(|_| {
            AppUpdateError::new(
                "app_update_restart_failed",
                "the updated application could not be started",
            )
        })
    }

    pub(crate) fn confirm_current_version(
        &self,
        current_version: &str,
        updates_dir: &Path,
    ) -> AppUpdateResult<()> {
        let marker_path = updates_dir.join(PENDING_INSTALL_FILE);
        let bytes = match fs::read(&marker_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => {
                return Err(AppUpdateError::new(
                    "app_update_install_marker_read_failed",
                    "the previous update marker could not be read",
                ));
            }
        };
        let marker: PendingInstall = serde_json::from_slice(&bytes).map_err(|_| {
            AppUpdateError::new(
                "app_update_install_marker_invalid",
                "the previous update marker is invalid",
            )
        })?;
        let target = self.target_path.as_deref().ok_or_else(|| {
            AppUpdateError::new(
                "app_update_install_marker_target_invalid",
                "the previous update marker does not match this installation",
            )
        })?;
        let expected_backup = appimage_backup_path(target)?;
        if marker.target != target || marker.backup != expected_backup {
            return Err(AppUpdateError::new(
                "app_update_install_marker_target_invalid",
                "the previous update marker does not match this installation",
            ));
        }
        if marker.version != current_version {
            if !marker.target.exists() && marker.backup.exists() {
                fs::rename(&marker.backup, &marker.target).map_err(|_| {
                    AppUpdateError::new(
                        "app_update_backup_restore_failed",
                        "the previous application backup could not be restored",
                    )
                })?;
                if let Some(parent) = marker.target.parent() {
                    sync_directory(parent)?;
                }
            } else if marker.backup.exists() {
                fs::remove_file(&marker.backup).map_err(|_| {
                    AppUpdateError::new(
                        "app_update_backup_cleanup_failed",
                        "the incomplete application backup could not be removed",
                    )
                })?;
            }
        }
        if marker.backup.exists() {
            fs::remove_file(&marker.backup).map_err(|_| {
                AppUpdateError::new(
                    "app_update_backup_cleanup_failed",
                    "the previous application backup could not be removed",
                )
            })?;
        }
        fs::remove_file(marker_path).map_err(|_| {
            AppUpdateError::new(
                "app_update_install_marker_cleanup_failed",
                "the completed update marker could not be removed",
            )
        })
    }

    fn from_package_override(package: &str) -> Self {
        let kind = match package {
            "appimage" => InstallationKind::AppImage,
            "deb" => InstallationKind::Deb,
            "rpm" => InstallationKind::Rpm,
            "flatpak" => InstallationKind::Flatpak,
            "app" => InstallationKind::MacApp,
            "mac_app_store" => InstallationKind::MacAppStore,
            "nsis" | "msi" => InstallationKind::WindowsInstaller,
            "windows_store" => InstallationKind::WindowsStore,
            _ => InstallationKind::Unmanaged,
        };
        Self {
            kind,
            package: package.to_string(),
            target_path: std::env::current_exe().ok(),
        }
    }
}

fn replace_appimage(
    target: &Path,
    staged_path: &Path,
    version: &str,
    updates_dir: &Path,
) -> AppUpdateResult<()> {
    let parent = target.parent().ok_or_else(|| {
        AppUpdateError::new(
            "app_update_install_target_parent_missing",
            "the application package has no parent directory",
        )
    })?;
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            AppUpdateError::new(
                "app_update_install_target_invalid",
                "the application package path is invalid",
            )
        })?;
    let replacement = parent.join(format!(".{file_name}.vibex-update-{}", std::process::id()));
    let backup = appimage_backup_path(target)?;
    if backup.exists() {
        return Err(AppUpdateError::new(
            "app_update_backup_already_exists",
            "a previous application backup still requires recovery or cleanup",
        ));
    }
    fs::copy(staged_path, &replacement).map_err(|_| {
        AppUpdateError::new(
            "app_update_install_copy_failed",
            "the verified update could not be copied beside the application",
        )
    })?;
    fs::metadata(target)
        .and_then(|metadata| {
            let permissions = metadata.permissions();
            fs::set_permissions(&replacement, permissions)
        })
        .map_err(|_| {
            let _ = fs::remove_file(&replacement);
            AppUpdateError::new(
                "app_update_install_permissions_failed",
                "the updated application permissions could not be prepared",
            )
        })?;
    if let Err(error) = sync_file(&replacement) {
        let _ = fs::remove_file(&replacement);
        return Err(error);
    }
    if fs::hard_link(target, &backup).is_err() {
        fs::copy(target, &backup).map_err(|_| {
            let _ = fs::remove_file(&replacement);
            AppUpdateError::new(
                "app_update_install_backup_failed",
                "the current application could not be backed up",
            )
        })?;
        if let Err(error) = sync_file(&backup) {
            let _ = fs::remove_file(&backup);
            let _ = fs::remove_file(&replacement);
            return Err(error);
        }
    }
    if fs::create_dir_all(updates_dir).is_err() {
        let _ = fs::remove_file(&backup);
        let _ = fs::remove_file(&replacement);
        return Err(AppUpdateError::new(
            "app_update_install_marker_create_failed",
            "the update recovery marker directory could not be created",
        ));
    }
    let marker_path = updates_dir.join(PENDING_INSTALL_FILE);
    if let Err(error) = write_json_atomic(
        &marker_path,
        &PendingInstall {
            version: version.to_string(),
            target: target.to_path_buf(),
            backup: backup.clone(),
        },
    ) {
        let _ = fs::remove_file(&backup);
        let _ = fs::remove_file(&replacement);
        return Err(error);
    }
    if fs::rename(&replacement, target).is_err() {
        let _ = fs::remove_file(&marker_path);
        let _ = fs::remove_file(&backup);
        let _ = fs::remove_file(&replacement);
        return Err(AppUpdateError::new(
            "app_update_install_replace_failed",
            "the application could not be replaced; the previous version was preserved",
        ));
    }
    if let Err(error) = sync_directory(parent) {
        let restored = fs::rename(&backup, target).is_ok();
        let _ = sync_directory(parent);
        let _ = fs::remove_file(&marker_path);
        if restored {
            return Err(AppUpdateError::new(
                "app_update_install_sync_failed",
                "the application update was not committed to disk; the previous version was restored",
            ));
        }
        return Err(error);
    }
    Ok(())
}

fn appimage_backup_path(target: &Path) -> AppUpdateResult<PathBuf> {
    let parent = target.parent().ok_or_else(|| {
        AppUpdateError::new(
            "app_update_install_target_parent_missing",
            "the application package has no parent directory",
        )
    })?;
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            AppUpdateError::new(
                "app_update_install_target_invalid",
                "the application package path is invalid",
            )
        })?;
    Ok(parent.join(format!(".{file_name}.vibex-backup")))
}

fn launch_system_installer(path: &Path) -> AppUpdateResult<()> {
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let result = Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(path).spawn();
    #[cfg(not(any(unix, target_os = "windows")))]
    let result: std::io::Result<std::process::Child> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unsupported platform",
    ));
    result.map(|_| ()).map_err(|_| {
        AppUpdateError::new(
            "app_update_system_installer_launch_failed",
            "the system installer could not be opened",
        )
    })
}

fn sync_file(path: &Path) -> AppUpdateResult<()> {
    OpenOptions::new()
        .read(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| {
            AppUpdateError::new(
                "app_update_install_sync_failed",
                "the updated application could not be committed to disk",
            )
        })
}

fn sync_directory(path: &Path) -> AppUpdateResult<()> {
    #[cfg(unix)]
    {
        OpenOptions::new()
            .read(true)
            .open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| {
                AppUpdateError::new(
                    "app_update_install_sync_failed",
                    "the application directory could not be committed to disk",
                )
            })?;
    }
    Ok(())
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> AppUpdateResult<()> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec(value).map_err(|_| {
        AppUpdateError::new(
            "app_update_install_marker_invalid",
            "the update recovery marker could not be encoded",
        )
    })?;
    fs::write(&temporary, bytes).map_err(|_| {
        AppUpdateError::new(
            "app_update_install_marker_write_failed",
            "the update recovery marker could not be written",
        )
    })?;
    sync_file(&temporary)?;
    fs::rename(&temporary, path).map_err(|_| {
        let _ = fs::remove_file(&temporary);
        AppUpdateError::new(
            "app_update_install_marker_replace_failed",
            "the update recovery marker could not be committed",
        )
    })?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn mac_app_root(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|ancestor| {
            ancestor
                .extension()
                .is_some_and(|extension| extension == "app")
        })
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::tempdir;

    use super::*;

    #[test]
    #[cfg(unix)]
    fn appimage_replacement_retains_backup_until_new_version_is_confirmed() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("Vibex.AppImage");
        let staged = directory.path().join("download.AppImage");
        let updates = directory.path().join("updates");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&staged, b"new").unwrap();

        replace_appimage(&target, &staged, "0.2.0", &updates).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        let backup = directory.path().join(".Vibex.AppImage.vibex-backup");
        assert_eq!(fs::read(&backup).unwrap(), b"old");

        let installation = Installation {
            kind: InstallationKind::AppImage,
            package: "appimage".to_string(),
            target_path: Some(target),
        };
        installation
            .confirm_current_version("0.2.0", &updates)
            .unwrap();
        assert!(!backup.exists());
        assert!(!updates.join(PENDING_INSTALL_FILE).exists());
    }

    #[test]
    fn incomplete_appimage_replacement_restores_the_previous_package() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("Vibex.AppImage");
        let backup = appimage_backup_path(&target).unwrap();
        let updates = directory.path().join("updates");
        fs::create_dir_all(&updates).unwrap();
        fs::write(&backup, b"old").unwrap();
        write_json_atomic(
            &updates.join(PENDING_INSTALL_FILE),
            &PendingInstall {
                version: "0.2.0".to_string(),
                target: target.clone(),
                backup,
            },
        )
        .unwrap();

        let installation = Installation {
            kind: InstallationKind::AppImage,
            package: "appimage".to_string(),
            target_path: Some(target.clone()),
        };
        installation
            .confirm_current_version("0.1.0", &updates)
            .unwrap();

        assert_eq!(fs::read(target).unwrap(), b"old");
        assert!(!updates.join(PENDING_INSTALL_FILE).exists());
    }

    #[test]
    fn update_marker_cannot_redirect_backup_cleanup() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("Vibex.AppImage");
        let unrelated = directory.path().join("unrelated");
        let updates = directory.path().join("updates");
        fs::create_dir_all(&updates).unwrap();
        fs::write(&target, b"new").unwrap();
        fs::write(&unrelated, b"keep").unwrap();
        write_json_atomic(
            &updates.join(PENDING_INSTALL_FILE),
            &PendingInstall {
                version: "0.2.0".to_string(),
                target: target.clone(),
                backup: unrelated.clone(),
            },
        )
        .unwrap();

        let installation = Installation {
            kind: InstallationKind::AppImage,
            package: "appimage".to_string(),
            target_path: Some(target),
        };
        let error = installation
            .confirm_current_version("0.2.0", &updates)
            .unwrap_err();

        assert_eq!(error.code, "app_update_install_marker_target_invalid");
        assert_eq!(fs::read(unrelated).unwrap(), b"keep");
    }
}

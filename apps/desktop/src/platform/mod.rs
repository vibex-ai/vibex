use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "linux")]
use std::{borrow::Cow, collections::BTreeSet};

use gpui::App;
use vibex_core::{VibexError, VibexResult};

pub const DESKTOP_UI_STATE_FILE: &str = "desktop-ui-state.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StorageUsage {
    pub database_bytes: u64,
    pub session_bytes: u64,
    pub terminal_bytes: u64,
    pub attachment_bytes: u64,
    pub diagnostic_bytes: u64,
    pub agent_installation_bytes: u64,
    pub other_bytes: u64,
}

impl StorageUsage {
    pub const fn total_bytes(self) -> u64 {
        self.database_bytes
            .saturating_add(self.session_bytes)
            .saturating_add(self.terminal_bytes)
            .saturating_add(self.attachment_bytes)
            .saturating_add(self.diagnostic_bytes)
            .saturating_add(self.agent_installation_bytes)
            .saturating_add(self.other_bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeSurfaceCapabilities {
    pub pdf_preview: bool,
    pub office_preview: bool,
}

impl NativeSurfaceCapabilities {
    pub const FOUNDATION: Self = Self {
        pdf_preview: false,
        office_preview: false,
    };
}

pub trait NativeSurfaceHost: Send + Sync {
    fn capabilities(&self) -> NativeSurfaceCapabilities;
}

#[derive(Debug, Default)]
pub struct FoundationNativeSurfaceHost;

impl NativeSurfaceHost for FoundationNativeSurfaceHost {
    fn capabilities(&self) -> NativeSurfaceCapabilities {
        NativeSurfaceCapabilities::FOUNDATION
    }
}

pub fn ui_state_path(home: &Path) -> PathBuf {
    home.join(DESKTOP_UI_STATE_FILE)
}

pub fn storage_usage(home: &Path) -> VibexResult<StorageUsage> {
    ensure_existing_absolute_path(home)?;
    let mut usage = StorageUsage::default();
    collect_storage_usage(home, home, &mut usage).map_err(|error| {
        VibexError::storage(
            "desktop_storage_usage_failed",
            "failed to inspect desktop storage usage",
        )
        .with_diagnostic("errorKind", format!("{:?}", error.kind()))
    })?;
    Ok(usage)
}

fn collect_storage_usage(root: &Path, path: &Path, usage: &mut StorageUsage) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_storage_usage(root, &path, usage)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let bytes = entry.metadata()?.len();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let folded = relative.to_string_lossy().to_ascii_lowercase();
        match storage_bucket(&folded) {
            StorageBucket::Database => {
                usage.database_bytes = usage.database_bytes.saturating_add(bytes)
            }
            StorageBucket::Attachment => {
                usage.attachment_bytes = usage.attachment_bytes.saturating_add(bytes)
            }
            StorageBucket::Diagnostic => {
                usage.diagnostic_bytes = usage.diagnostic_bytes.saturating_add(bytes)
            }
            StorageBucket::AgentInstallation => {
                usage.agent_installation_bytes =
                    usage.agent_installation_bytes.saturating_add(bytes)
            }
            StorageBucket::Terminal => {
                usage.terminal_bytes = usage.terminal_bytes.saturating_add(bytes)
            }
            StorageBucket::Session => {
                usage.session_bytes = usage.session_bytes.saturating_add(bytes)
            }
            StorageBucket::Other => usage.other_bytes = usage.other_bytes.saturating_add(bytes),
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageBucket {
    Database,
    Session,
    Terminal,
    Attachment,
    Diagnostic,
    AgentInstallation,
    Other,
}

fn storage_bucket(relative_path: &str) -> StorageBucket {
    if relative_path.ends_with("vibex.db")
        || relative_path.ends_with("vibex.db-wal")
        || relative_path.ends_with("vibex.db-shm")
    {
        StorageBucket::Database
    } else if relative_path == "acp-agents" || relative_path.starts_with("acp-agents/") {
        StorageBucket::AgentInstallation
    } else if relative_path.contains("attachment") || relative_path.contains("upload") {
        StorageBucket::Attachment
    } else if relative_path.contains("diagnostic")
        || relative_path.contains("backup")
        || relative_path.contains("log")
    {
        StorageBucket::Diagnostic
    } else if relative_path.contains("terminal") || relative_path.contains("pty") {
        StorageBucket::Terminal
    } else if relative_path.contains("session") || relative_path.contains("transcript") {
        StorageBucket::Session
    } else {
        StorageBucket::Other
    }
}

pub fn set_launch_at_login(enabled: bool, application_id: &str) -> VibexResult<()> {
    let target = launch_at_login_path(application_id)?;
    if !enabled {
        match fs::remove_file(&target) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(VibexError::storage(
                    "desktop_launch_at_login_remove_failed",
                    "failed to disable launch at login",
                )
                .with_diagnostic("errorKind", format!("{:?}", error.kind())));
            }
        }
    }
    let executable = env::current_exe().map_err(|error| {
        VibexError::process(
            "desktop_launch_at_login_executable_failed",
            "failed to resolve the Vibex executable",
        )
        .with_diagnostic("errorKind", format!("{:?}", error.kind()))
    })?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            VibexError::storage(
                "desktop_launch_at_login_directory_failed",
                "failed to prepare the launch-at-login directory",
            )
            .with_diagnostic("errorKind", format!("{:?}", error.kind()))
        })?;
    }
    fs::write(
        &target,
        launch_at_login_contents(&executable, application_id),
    )
    .map_err(|error| {
        VibexError::storage(
            "desktop_launch_at_login_write_failed",
            "failed to enable launch at login",
        )
        .with_diagnostic("errorKind", format!("{:?}", error.kind()))
    })
}

pub fn launch_at_login_enabled(application_id: &str) -> bool {
    launch_at_login_path(application_id).is_ok_and(|path| path.is_file())
}

fn launch_at_login_path(application_id: &str) -> VibexResult<PathBuf> {
    if application_id.is_empty()
        || !application_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-'))
    {
        return Err(VibexError::validation(
            "desktop_launch_at_login_application_id_invalid",
            "application id is invalid",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        let config = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or_else(|| {
                VibexError::storage(
                    "desktop_launch_at_login_home_missing",
                    "desktop configuration directory is unavailable",
                )
            })?;
        return Ok(config
            .join("autostart")
            .join(format!("{application_id}.desktop")));
    }
    #[cfg(target_os = "macos")]
    {
        let home = env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            VibexError::storage(
                "desktop_launch_at_login_home_missing",
                "home directory is unavailable",
            )
        })?;
        return Ok(home
            .join("Library/LaunchAgents")
            .join(format!("{application_id}.plist")));
    }
    #[cfg(target_os = "windows")]
    {
        let app_data = env::var_os("APPDATA").map(PathBuf::from).ok_or_else(|| {
            VibexError::storage(
                "desktop_launch_at_login_home_missing",
                "application data directory is unavailable",
            )
        })?;
        return Ok(app_data
            .join("Microsoft/Windows/Start Menu/Programs/Startup")
            .join(format!("{application_id}.cmd")));
    }
}

#[cfg(target_os = "linux")]
fn launch_at_login_contents(executable: &Path, _: &str) -> Vec<u8> {
    format!(
        "[Desktop Entry]\nType=Application\nName=Vibex\nExec=\"{}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        desktop_exec_escape(&executable.to_string_lossy())
    )
    .into_bytes()
}

#[cfg(target_os = "linux")]
fn desktop_exec_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
        .replace('%', "%%")
}

#[cfg(target_os = "macos")]
fn launch_at_login_contents(executable: &Path, application_id: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>Label</key><string>{application_id}</string><key>ProgramArguments</key><array><string>{}</string></array><key>RunAtLoad</key><true/></dict></plist>\n",
        xml_escape(&executable.to_string_lossy())
    )
    .into_bytes()
}

#[cfg(any(target_os = "macos", test))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "windows")]
fn launch_at_login_contents(executable: &Path, _: &str) -> Vec<u8> {
    format!(
        "@echo off\r\nstart \"\" \"{}\"\r\n",
        windows_batch_escape(&executable.to_string_lossy())
    )
    .into_bytes()
}

#[cfg(any(target_os = "windows", test))]
fn windows_batch_escape(value: &str) -> String {
    value.replace('%', "%%")
}

pub fn send_system_notification(title: &str, body: &str) -> VibexResult<()> {
    let title = bounded_notification_text(title, 80)?;
    let body = bounded_notification_text(body, 240)?;
    #[cfg(target_os = "linux")]
    {
        return spawn_open_command(
            Command::new("notify-send").args(["--app-name=Vibex", &title, &body]),
            "desktop_notification_failed",
            "failed to show a system notification",
        );
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification {} with title {}",
            apple_script_string(&body),
            apple_script_string(&title)
        );
        return spawn_open_command(
            Command::new("osascript").args(["-e", &script]),
            "desktop_notification_failed",
            "failed to show a system notification",
        );
    }
    #[cfg(target_os = "windows")]
    {
        let script = format!(
            "$w=New-Object -ComObject WScript.Shell;$w.Popup('{}',8,'{}',64)",
            body.replace('\'', "''"),
            title.replace('\'', "''")
        );
        return spawn_open_command(
            Command::new("powershell.exe").args(["-NoProfile", "-Command", &script]),
            "desktop_notification_failed",
            "failed to show a system notification",
        );
    }
}

fn bounded_notification_text(value: &str, limit: usize) -> VibexResult<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(VibexError::validation(
            "desktop_notification_text_invalid",
            "notification text is invalid",
        ));
    }
    Ok(value.chars().take(limit).collect())
}

#[cfg(target_os = "macos")]
fn apple_script_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn platform_font_fallbacks() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        &[
            ".SystemUIFont",
            "PingFang SC",
            "Hiragino Sans",
            "sans-serif",
        ]
    }
    #[cfg(target_os = "windows")]
    {
        &["Segoe UI", "Microsoft YaHei UI", "sans-serif"]
    }
    #[cfg(target_os = "linux")]
    {
        &[
            "Inter Variable",
            "Noto Sans CJK SC",
            "DejaVu Sans",
            "sans-serif",
        ]
    }
}

/// Loads only the selected system font families into GPUI.
///
/// Linux GPUI starts with an empty font database so calling this function does
/// not fall back to loading every font installed on the machine. Fontconfig
/// resolves the requested family to its matching files, and those files alone
/// are copied into the process.
pub fn load_selected_font_families(cx: &mut App, families: &[&str]) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let mut paths = Vec::new();
        let mut seen_paths = BTreeSet::new();
        for family in families {
            for path in system_font_paths(family) {
                if seen_paths.insert(path.clone()) {
                    paths.push(path);
                }
            }
        }

        if paths.is_empty() {
            return Ok(());
        }

        let mut font_data = Vec::new();
        let mut paths_to_mark = Vec::new();
        for path in paths {
            if cx
                .try_global::<LoadedSystemFontPaths>()
                .is_some_and(|loaded| loaded.0.contains(&path))
            {
                continue;
            }
            let data = fs::read(&path).map_err(|error| {
                format!("failed to read system font {}: {error}", path.display())
            })?;
            font_data.push(Cow::Owned(data));
            paths_to_mark.push(path);
        }

        if font_data.is_empty() {
            return Ok(());
        }

        cx.text_system()
            .add_fonts(font_data)
            .map_err(|error| format!("failed to load selected system fonts: {error}"))?;
        cx.default_global::<LoadedSystemFontPaths>()
            .0
            .extend(paths_to_mark);
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (cx, families);
        Ok(())
    }
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct LoadedSystemFontPaths(BTreeSet<PathBuf>);

#[cfg(target_os = "linux")]
impl gpui::Global for LoadedSystemFontPaths {}

#[cfg(target_os = "linux")]
fn system_font_paths(family: &str) -> Vec<PathBuf> {
    let family = family.trim();
    if family.is_empty()
        || family.len() > 160
        || family.chars().any(char::is_control)
        || is_bundled_font_family(family)
        || is_generic_font_family(family)
    {
        return Vec::new();
    }

    let mut paths = BTreeSet::new();
    for pattern in [
        family.to_string(),
        format!("{family}:weight=bold"),
        format!("{family}:slant=italic"),
        format!("{family}:weight=bold:slant=italic"),
    ] {
        if let Some(path) = fc_match_path(&pattern, family) {
            paths.insert(path);
        }
    }

    paths.into_iter().collect()
}

#[cfg(target_os = "linux")]
fn fc_match_path(pattern: &str, requested_family: &str) -> Option<PathBuf> {
    let output = Command::new("fc-match")
        .args(["--format=%{family}\t%{file}\n", "--", pattern])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let line = String::from_utf8_lossy(&output.stdout);
    let (matched_family, path) = line.lines().next()?.split_once('\t')?;
    if !font_family_matches(requested_family, matched_family) {
        return None;
    }
    let path = PathBuf::from(path.trim());
    path.is_file().then_some(path)
}

#[cfg(target_os = "linux")]
fn font_family_matches(requested: &str, matched: &str) -> bool {
    matched
        .split(',')
        .any(|candidate| candidate.trim().eq_ignore_ascii_case(requested))
}

#[cfg(target_os = "linux")]
fn is_generic_font_family(family: &str) -> bool {
    matches!(
        family.trim().to_ascii_lowercase().as_str(),
        "sans-serif" | "serif" | "monospace" | "cursive" | "fantasy" | "system-ui"
    )
}

#[cfg(target_os = "linux")]
fn is_bundled_font_family(family: &str) -> bool {
    matches!(
        family.trim().to_ascii_lowercase().as_str(),
        "inter variable" | "ibm plex sans" | "lilex"
    )
}

pub fn default_code_font_family() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Menlo"
    }
    #[cfg(target_os = "windows")]
    {
        "Consolas"
    }
    #[cfg(target_os = "linux")]
    {
        "monospace"
    }
}

pub fn system_font_families(_cx: &App) -> Vec<String> {
    #[cfg(target_os = "linux")]
    let discovered = discover_system_font_families();
    #[cfg(not(target_os = "linux"))]
    let discovered = _cx.text_system().all_font_names();

    normalize_font_families(
        discovered
            .into_iter()
            .chain(
                platform_font_fallbacks()
                    .iter()
                    .map(|name| (*name).to_string()),
            )
            .chain([
                "Inter Variable".to_string(),
                default_code_font_family().to_string(),
            ]),
    )
}

#[cfg(target_os = "linux")]
fn discover_system_font_families() -> Vec<String> {
    // This asks fontconfig for names only. The child process may inspect its
    // own caches, but no font files are loaded into Vibex's address space until
    // the user selects a family through `load_selected_font_families`.
    let output = Command::new("fc-list")
        .args(["--format=%{family}\n"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter(|family| !family.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalOpenTool {
    pub id: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct ExternalOpenToolDefinition {
    id: &'static str,
    label: &'static str,
    commands: &'static [&'static str],
    mac_apps: &'static [&'static str],
}

const EXTERNAL_OPEN_TOOLS: &[ExternalOpenToolDefinition] = &[
    ExternalOpenToolDefinition {
        id: "vscode",
        label: "VS Code",
        commands: &["code"],
        mac_apps: &["Visual Studio Code"],
    },
    ExternalOpenToolDefinition {
        id: "vscode_insiders",
        label: "VS Code Insiders",
        commands: &["code-insiders"],
        mac_apps: &["Visual Studio Code - Insiders"],
    },
    ExternalOpenToolDefinition {
        id: "cursor",
        label: "Cursor",
        commands: &["cursor"],
        mac_apps: &["Cursor"],
    },
    ExternalOpenToolDefinition {
        id: "windsurf",
        label: "Windsurf",
        commands: &["windsurf"],
        mac_apps: &["Windsurf"],
    },
    ExternalOpenToolDefinition {
        id: "zed",
        label: "Zed",
        commands: &["zed"],
        mac_apps: &["Zed"],
    },
    ExternalOpenToolDefinition {
        id: "intellij",
        label: "IntelliJ IDEA",
        commands: &["idea"],
        mac_apps: &["IntelliJ IDEA Ultimate", "IntelliJ IDEA"],
    },
    ExternalOpenToolDefinition {
        id: "webstorm",
        label: "WebStorm",
        commands: &["webstorm"],
        mac_apps: &["WebStorm"],
    },
    ExternalOpenToolDefinition {
        id: "pycharm",
        label: "PyCharm",
        commands: &["pycharm"],
        mac_apps: &["PyCharm"],
    },
    ExternalOpenToolDefinition {
        id: "goland",
        label: "GoLand",
        commands: &["goland"],
        mac_apps: &["GoLand"],
    },
    ExternalOpenToolDefinition {
        id: "rustrover",
        label: "RustRover",
        commands: &["rustrover"],
        mac_apps: &["RustRover"],
    },
    ExternalOpenToolDefinition {
        id: "clion",
        label: "CLion",
        commands: &["clion"],
        mac_apps: &["CLion"],
    },
    ExternalOpenToolDefinition {
        id: "phpstorm",
        label: "PhpStorm",
        commands: &["phpstorm"],
        mac_apps: &["PhpStorm"],
    },
    ExternalOpenToolDefinition {
        id: "rider",
        label: "Rider",
        commands: &["rider"],
        mac_apps: &["Rider"],
    },
    ExternalOpenToolDefinition {
        id: "fleet",
        label: "Fleet",
        commands: &["fleet"],
        mac_apps: &["Fleet"],
    },
    ExternalOpenToolDefinition {
        id: "sublime_text",
        label: "Sublime Text",
        commands: &["subl"],
        mac_apps: &["Sublime Text"],
    },
    ExternalOpenToolDefinition {
        id: "xcode",
        label: "Xcode",
        commands: &["xed"],
        mac_apps: &["Xcode"],
    },
];

pub fn available_external_tools() -> Vec<ExternalOpenTool> {
    EXTERNAL_OPEN_TOOLS
        .iter()
        .filter(|tool| {
            tool.commands
                .iter()
                .any(|command| find_executable(command).is_some())
                || find_mac_application(tool.mac_apps).is_some()
        })
        .map(|tool| ExternalOpenTool {
            id: tool.id,
            label: tool.label,
        })
        .collect()
}

pub fn reveal_path_in_file_manager(path: &Path) -> VibexResult<()> {
    ensure_existing_absolute_path(path)?;
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        if path.is_dir() {
            command.arg(path);
        } else {
            command.args(["-R"]).arg(path);
        }
        return spawn_open_command(
            &mut command,
            "file_open_file_manager_failed",
            "failed to open path in the file manager",
        );
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("explorer");
        if path.is_dir() {
            command.arg(path);
        } else {
            command.arg(format!("/select,{}", path.display()));
        }
        return spawn_open_command(
            &mut command,
            "file_open_file_manager_failed",
            "failed to open path in the file manager",
        );
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let target = if path.is_dir() {
            path
        } else {
            path.parent().ok_or_else(|| {
                VibexError::validation(
                    "file_open_file_manager_parent_missing",
                    "path does not have a parent directory",
                )
            })?
        };
        open_path_with_default_app(target)
    }
}

pub fn open_path_with_default_app(path: &Path) -> VibexResult<()> {
    ensure_existing_absolute_path(path)?;
    #[cfg(target_os = "macos")]
    {
        return spawn_open_command(
            Command::new("open").arg(path),
            "file_open_default_app_failed",
            "failed to open path in the default app",
        );
    }
    #[cfg(target_os = "windows")]
    {
        return spawn_open_command(
            Command::new("cmd").args(["/C", "start", ""]).arg(path),
            "file_open_default_app_failed",
            "failed to open path in the default app",
        );
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        for (program, args) in [
            ("xdg-open", &[][..]),
            ("gio", &["open"][..]),
            ("kde-open", &[][..]),
            ("gnome-open", &[][..]),
        ] {
            let mut command = Command::new(program);
            command.args(args).arg(path);
            match command.spawn() {
                Ok(_) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(VibexError::process(
                        "file_open_default_app_failed",
                        "failed to open path in the default app",
                    )
                    .with_diagnostic("program", program)
                    .with_diagnostic("error", error.to_string()));
                }
            }
        }
        Err(VibexError::process(
            "file_open_default_app_unavailable",
            "no default application opener is available",
        ))
    }
}

/// Opens an already validated HTTP(S) URL in the system browser.  URL scheme,
/// host, and credential checks happen at the management runtime boundary;
/// this helper only performs the platform launch and never creates an embed.
pub fn open_external_url(url: &str) -> VibexResult<()> {
    if url.trim().is_empty() {
        return Err(VibexError::validation(
            "external_url_empty",
            "external URL must not be empty",
        ));
    }
    #[cfg(target_os = "macos")]
    {
        return spawn_open_command(
            Command::new("open").arg(url),
            "external_url_open_failed",
            "failed to open URL in the system browser",
        );
    }
    #[cfg(target_os = "windows")]
    {
        return spawn_open_command(
            Command::new("rundll32.exe")
                .args(["url.dll,FileProtocolHandler"])
                .arg(url),
            "external_url_open_failed",
            "failed to open URL in the system browser",
        );
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        for program in ["xdg-open", "gio", "kde-open", "gnome-open"] {
            let mut command = Command::new(program);
            if program == "gio" {
                command.arg("open");
            }
            match command.arg(url).spawn() {
                Ok(_) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(VibexError::process(
                        "external_url_open_failed",
                        "failed to open URL in the system browser",
                    )
                    .with_diagnostic("program", program)
                    .with_diagnostic("error", error.to_string()));
                }
            }
        }
        Err(VibexError::process(
            "external_url_open_unavailable",
            "no system browser opener is available",
        ))
    }
}

pub fn open_native_terminal_for_path(path: &Path) -> VibexResult<()> {
    ensure_existing_absolute_path(path)?;
    let cwd = if path.is_dir() {
        path
    } else {
        path.parent().ok_or_else(|| {
            VibexError::validation(
                "file_open_native_terminal_parent_missing",
                "path does not have a parent directory",
            )
        })?
    };
    #[cfg(target_os = "macos")]
    {
        return spawn_open_command(
            Command::new("open").args(["-a", "Terminal"]).arg(cwd),
            "file_open_native_terminal_failed",
            "failed to open the native terminal",
        );
    }
    #[cfg(target_os = "windows")]
    {
        return spawn_open_command(
            Command::new("cmd")
                .args(["/C", "start", "", "cmd", "/K", "cd", "/d"])
                .arg(cwd),
            "file_open_native_terminal_failed",
            "failed to open the native terminal",
        );
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let configured = env::var("TERMINAL")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let mut candidates = configured.into_iter().collect::<Vec<_>>();
        candidates.extend(
            [
                "x-terminal-emulator",
                "gnome-terminal",
                "ghostty",
                "kgx",
                "konsole",
                "wezterm",
                "alacritty",
                "kitty",
                "xterm",
            ]
            .into_iter()
            .map(str::to_string),
        );
        candidates.sort();
        candidates.dedup();
        for program in candidates {
            match Command::new(&program).current_dir(cwd).spawn() {
                Ok(_) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(VibexError::process(
                        "file_open_native_terminal_failed",
                        "failed to open the native terminal",
                    )
                    .with_diagnostic("program", program)
                    .with_diagnostic("error", error.to_string()));
                }
            }
        }
        Err(VibexError::process(
            "file_open_native_terminal_unavailable",
            "no native terminal emulator is available",
        ))
    }
}

pub fn open_path_with_external_tool(tool_id: &str, path: &Path) -> VibexResult<()> {
    ensure_existing_absolute_path(path)?;
    let tool = EXTERNAL_OPEN_TOOLS
        .iter()
        .find(|tool| tool.id == tool_id)
        .ok_or_else(|| {
            VibexError::validation("file_open_tool_unknown", "unknown file open tool")
                .with_diagnostic("toolId", tool_id)
        })?;
    if let Some(executable) = tool
        .commands
        .iter()
        .find_map(|command| find_executable(command))
    {
        return spawn_open_command(
            Command::new(executable).arg(path),
            "file_open_tool_failed",
            "failed to open path with the selected tool",
        );
    }
    #[cfg(target_os = "macos")]
    if let Some(app_name) = find_mac_application(tool.mac_apps) {
        return spawn_open_command(
            Command::new("open")
                .args(["-a", app_name.as_str()])
                .arg(path),
            "file_open_tool_failed",
            "failed to open path with the selected tool",
        );
    }
    Err(VibexError::process(
        "file_open_tool_unavailable",
        "selected open tool is unavailable",
    )
    .with_diagnostic("toolId", tool.id))
}

fn ensure_existing_absolute_path(path: &Path) -> VibexResult<()> {
    if !path.is_absolute() || !path.exists() {
        return Err(VibexError::validation(
            "file_open_path_invalid",
            "file open target must be an existing absolute path",
        ));
    }
    Ok(())
}

fn find_executable(program: &str) -> Option<PathBuf> {
    let program_path = Path::new(program);
    if program_path.components().count() > 1 && program_path.is_file() {
        return Some(program_path.to_path_buf());
    }
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .flat_map(|directory| {
            executable_candidates(program)
                .into_iter()
                .map(move |name| directory.join(name))
        })
        .find(|candidate| candidate.is_file())
}

#[cfg(target_os = "macos")]
fn find_mac_application(app_names: &[&'static str]) -> Option<String> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Ok(home) = env::var("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    for app_name in app_names {
        for root in &roots {
            if root.join(format!("{app_name}.app")).exists() {
                return Some((*app_name).to_string());
            }
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn find_mac_application(_: &[&'static str]) -> Option<String> {
    None
}

#[cfg(target_os = "windows")]
fn executable_candidates(program: &str) -> Vec<String> {
    if Path::new(program).extension().is_some() {
        return vec![program.to_string()];
    }
    env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| format!("{program}{extension}"))
        .chain(std::iter::once(program.to_string()))
        .collect()
}

#[cfg(not(target_os = "windows"))]
fn executable_candidates(program: &str) -> Vec<String> {
    vec![program.to_string()]
}

fn spawn_open_command(
    command: &mut Command,
    code: &'static str,
    message: &'static str,
) -> VibexResult<()> {
    command.spawn().map(|_| ()).map_err(|error| {
        VibexError::process(code, message).with_diagnostic("error", error.to_string())
    })
}

fn normalize_font_families(fonts: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut by_folded_name = BTreeMap::new();
    for font in fonts {
        let name = font.trim();
        if name.is_empty() || name.len() > 160 || name.chars().any(char::is_control) {
            continue;
        }
        by_folded_name
            .entry(name.to_lowercase())
            .or_insert_with(|| name.to_string());
    }
    by_folded_name.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foundation_native_surfaces_fail_truthfully() {
        let host = FoundationNativeSurfaceHost;
        assert_eq!(host.capabilities(), NativeSurfaceCapabilities::FOUNDATION);
    }

    #[test]
    fn font_families_are_bounded_sorted_and_case_insensitively_unique() {
        assert_eq!(
            normalize_font_families([
                " Noto Sans ".to_string(),
                "inter variable".to_string(),
                "Inter Variable".to_string(),
                "".to_string(),
                "bad\nfont".to_string(),
            ]),
            vec!["inter variable".to_string(), "Noto Sans".to_string()]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn font_family_matching_accepts_fontconfig_aliases_without_substring_matches() {
        assert!(font_family_matches(
            "JetBrains Mono",
            "JetBrains Mono,JetBrains Mono Light"
        ));
        assert!(!font_family_matches("Mono", "JetBrains Mono"));
        assert!(is_bundled_font_family("IBM Plex Sans"));
        assert!(!is_bundled_font_family("JetBrains Mono"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn selected_font_lookup_ignores_missing_and_logical_families() {
        assert!(system_font_paths("Vibex Missing Font 9f2c").is_empty());
        assert!(system_font_paths("monospace").is_empty());
        assert!(system_font_paths("sans-serif").is_empty());

        let selected = system_font_paths("JetBrains Mono");
        assert!(selected.len() <= 4);
        assert!(selected.iter().all(|path| path.is_file()));
    }

    #[test]
    fn storage_bucket_prefers_specific_runtime_artifacts() {
        assert_eq!(storage_bucket("vibex.db-wal"), StorageBucket::Database);
        assert_eq!(
            storage_bucket("acp-agents/agents/codex/node_modules/index.js"),
            StorageBucket::AgentInstallation
        );
        assert_eq!(
            storage_bucket("clipboard-attachments/image.bin"),
            StorageBucket::Attachment
        );
        assert_eq!(
            storage_bucket("terminal-output.log"),
            StorageBucket::Diagnostic
        );
        assert_eq!(
            storage_bucket("sessions/transcript.json"),
            StorageBucket::Session
        );
        assert_eq!(storage_bucket("sessions/item.json"), StorageBucket::Session);
        assert_eq!(storage_bucket("misc.bin"), StorageBucket::Other);
    }

    #[test]
    fn storage_usage_total_includes_agent_installations() {
        let usage = StorageUsage {
            agent_installation_bytes: 7,
            ..StorageUsage::default()
        };
        assert_eq!(usage.total_bytes(), 7);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_autostart_contents_escape_exec_path() {
        let content = String::from_utf8(launch_at_login_contents(
            Path::new("/tmp/Vibex \\\"preview\\\"/vibex"),
            "dev.vibex.preview",
        ))
        .unwrap();
        assert!(content.contains("Exec=\"/tmp/Vibex \\\\\\\"preview\\\\\\\"/vibex\""));
    }

    #[test]
    fn macos_autostart_xml_escapes_executable_path() {
        assert_eq!(
            xml_escape("/Applications/Vibex & Preview <1>/vibex"),
            "/Applications/Vibex &amp; Preview &lt;1&gt;/vibex"
        );
    }

    #[test]
    fn windows_autostart_batch_escapes_percent_expansion() {
        assert_eq!(
            windows_batch_escape(r"C:\\Apps\\Vibex %preview%\\vibex.exe"),
            r"C:\\Apps\\Vibex %%preview%%\\vibex.exe"
        );
    }
}

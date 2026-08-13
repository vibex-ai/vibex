use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use gpui::App;
use vibex_core::{VibexError, VibexResult};

pub const DESKTOP_UI_STATE_FILE: &str = "desktop-ui-state.json";

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

pub fn system_font_families(cx: &App) -> Vec<String> {
    normalize_font_families(
        cx.text_system()
            .all_font_names()
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
}

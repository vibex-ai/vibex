//! What the interface remembers about itself between runs.
//!
//! The settings surface offers a theme, an appearance, an icon set, the landing
//! logo and its two effects, a language and a default workspace, and the
//! command palette leads with the commands the reader reached for last. None of
//! that is the runtime's to keep, so it is written beside the session-list
//! arrangement and the switcher's memory under the same home: a reader who
//! wants the interface to forget clears one directory.
//!
//! Every stored value is an optional string rather than an enum lifted from the
//! crate that owns it, and every two-state value is an optional boolean that is
//! absent until the reader moves the row. The file is hand-editable and has to
//! survive a value this build does not know: an unknown theme id, icon set,
//! appearance or language loads as "no choice" and the caller's own default
//! answers, so a file written by a newer build is not an error in an older one —
//! and a field an older build never wrote is the shipped behaviour rather than
//! the off state.

use std::path::Path;

use serde::{Deserialize, Serialize};
use vibex_desktop_model::ThemeSelection;
use vibex_ui::GpuiThemeMode;

use crate::locale::Locale;
use crate::logo::MarkStyle;
use crate::theme::GlyphMode;

/// How many palette commands the file keeps.
///
/// The list is a recency record, not a history. The bound is the one the
/// palette draws, so a remembered command the list stopped showing is not kept
/// forever.
pub const RECENT_COMMAND_LIMIT: usize = 8;

/// The interface's own remembered state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfacePreferences {
    /// The reader's theme per appearance, the way the desktop records it.
    ///
    /// Two slots rather than one id: a light palette and a dark one are two
    /// choices, and moving between appearances must not overwrite the other.
    #[serde(default, skip_serializing_if = "themes_are_empty")]
    pub themes: ThemeSelection,
    /// `dark` or `light`, when the reader moved the appearance off its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// `unicode` or `ascii`, when the reader chose an icon set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<String>,
    /// `classic` or `glitch`, when the reader chose a landing mark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<String>,
    /// Whether the mark animates while the page waits.
    ///
    /// Absent is not "off": it means the reader never moved the row, so the
    /// shipped default answers — which is why the two effects below are stored
    /// as the state they were left in rather than as the one that differs from
    /// the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<bool>,
    /// Whether a line scrambles when the text it shows changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transitions: Option<bool>,
    /// The BCP-47 tag of the chosen language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// The default workspace for new sessions, when the reader named one.
    ///
    /// Absent is not "no workspace": it means the client keeps answering with
    /// the directory it was started in, which is the answer a reader gave by
    /// choosing where to run the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Palette action ids, most recently used first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_commands: Vec<String>,
}

impl InterfacePreferences {
    /// Read what a previous run wrote, or remember nothing.
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    /// Persist what has been remembered. A failure is silent: losing a
    /// preference is not worth interrupting the reader, and the next change
    /// tries again.
    pub fn save(&self, path: Option<&Path>) {
        let Some(path) = path else {
            return;
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        if let Ok(body) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, body);
        }
    }

    /// The remembered appearance, when it is one this build knows.
    pub fn mode(&self) -> Option<GpuiThemeMode> {
        match self.mode.as_deref() {
            Some("dark") => Some(GpuiThemeMode::Dark),
            Some("light") => Some(GpuiThemeMode::Light),
            _ => None,
        }
    }

    /// The remembered icon set, when it is one this build knows.
    pub fn glyphs(&self) -> Option<GlyphMode> {
        match self.icons.as_deref() {
            Some("ascii") => Some(GlyphMode::Ascii),
            Some("unicode") => Some(GlyphMode::Unicode),
            _ => None,
        }
    }

    /// The remembered landing mark, when it is one this build knows.
    pub fn mark(&self) -> Option<MarkStyle> {
        self.mark.as_deref().and_then(MarkStyle::from_id)
    }

    /// Whether the mark animates, or the shipped default when unchosen.
    pub fn motion(&self) -> bool {
        self.motion.unwrap_or(true)
    }

    /// Whether a changing line scrambles, or the shipped default when unchosen.
    pub fn transitions(&self) -> bool {
        self.transitions.unwrap_or(true)
    }

    /// The remembered language, when it is one this build ships.
    pub fn locale(&self) -> Option<Locale> {
        match self.locale.as_deref() {
            Some("en") => Some(Locale::En),
            Some("zh-CN") => Some(Locale::ZhCn),
            Some("zh-TW") => Some(Locale::ZhTw),
            _ => None,
        }
    }

    /// Remember one command, most recent first.
    ///
    /// A command that was already in the list moves to the front rather than
    /// being written twice: the list is what the palette lifts to the top, and
    /// one row per command is the only shape that can be drawn.
    pub fn remember_command(&mut self, id: &str) {
        self.recent_commands.retain(|candidate| candidate != id);
        self.recent_commands.insert(0, id.to_string());
        self.recent_commands.truncate(RECENT_COMMAND_LIMIT);
    }
}

/// Whether both appearance slots are empty, so the field can be left out.
fn themes_are_empty(themes: &ThemeSelection) -> bool {
    themes.light().is_none() && themes.dark().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_round_trip_through_their_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-interface.json");
        let preferences = InterfacePreferences {
            themes: ThemeSelection {
                light: Some("vibex-light".to_string()),
                dark: Some("vibex-dark".to_string()),
            },
            mode: Some("light".to_string()),
            icons: Some("ascii".to_string()),
            mark: Some("glitch".to_string()),
            motion: Some(false),
            transitions: Some(false),
            locale: Some("zh-CN".to_string()),
            workspace: Some("/tmp/vibex-workspace".to_string()),
            recent_commands: vec!["open_settings".to_string()],
        };

        preferences.save(Some(&path));
        let loaded = InterfacePreferences::load(Some(&path));
        assert_eq!(loaded, preferences);
        assert_eq!(loaded.mode(), Some(GpuiThemeMode::Light));
        assert_eq!(loaded.glyphs(), Some(GlyphMode::Ascii));
        assert_eq!(loaded.mark(), Some(MarkStyle::Glitch));
        assert!(!loaded.motion());
        assert!(!loaded.transitions());
        assert_eq!(loaded.locale(), Some(Locale::ZhCn));
    }

    #[test]
    fn an_empty_selection_writes_no_theme_and_an_absent_file_is_empty_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-interface.json");
        InterfacePreferences::default().save(Some(&path));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            !body.contains("themes"),
            "an empty selection is not a choice:\n{body}"
        );

        assert_eq!(
            InterfacePreferences::load(Some(&path)),
            InterfacePreferences::default()
        );
        assert_eq!(
            InterfacePreferences::load(Some(&directory.path().join("absent.json"))),
            InterfacePreferences::default()
        );
        assert_eq!(
            InterfacePreferences::load(None),
            InterfacePreferences::default()
        );
    }

    #[test]
    fn a_value_this_build_does_not_know_loads_as_no_choice() {
        // A file written by a newer build, or edited by hand, must not be an
        // error and must not become a value this one sends anywhere.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-interface.json");
        std::fs::write(
            &path,
            r#"{
              "mode": "system",
              "icons": "emoji",
              "mark": "plaid",
              "locale": "fr-FR",
              "themes": { "dark": "future-dark" }
            }"#,
        )
        .unwrap();

        let loaded = InterfacePreferences::load(Some(&path));
        assert_eq!(loaded.mode(), None);
        assert_eq!(loaded.glyphs(), None);
        assert_eq!(loaded.mark(), None);
        assert_eq!(loaded.locale(), None);
        // A build that never wrote the two effects leaves them at the shipped
        // default rather than reading "absent" as "off".
        assert!(loaded.motion());
        assert!(loaded.transitions());
        // An unknown theme id is kept as written: resolution, not loading, is
        // what decides whether a palette exists.
        assert_eq!(loaded.themes.dark(), Some("future-dark"));
    }

    #[test]
    fn an_unreadable_file_remembers_nothing_rather_than_failing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tui-interface.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(
            InterfacePreferences::load(Some(&path)),
            InterfacePreferences::default()
        );
    }

    #[test]
    fn remembered_commands_are_most_recent_first_and_bounded() {
        let mut preferences = InterfacePreferences::default();
        for index in 0..RECENT_COMMAND_LIMIT + 4 {
            preferences.remember_command(&format!("command_{index}"));
        }
        assert_eq!(preferences.recent_commands.len(), RECENT_COMMAND_LIMIT);
        assert_eq!(preferences.recent_commands[0], "command_11");

        // Reaching for a command already in the list moves it to the front
        // instead of listing it twice.
        preferences.remember_command("command_4");
        assert_eq!(preferences.recent_commands[0], "command_4");
        assert_eq!(
            preferences
                .recent_commands
                .iter()
                .filter(|id| *id == "command_4")
                .count(),
            1
        );
    }
}

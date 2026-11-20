//! The app's device preferences.
//!
//! The GPUI client keeps `app-settings.json`; the `nsh` client keeps the same
//! small set in the engine's key-value storage, so a theme or a language chosen
//! on the phone survives a restart. Appearance is applied through `nsh`'s
//! color-scheme command, and language through the copy resolver both clients
//! share.

use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use nsh_core::{platform::Appearance, prelude::*, storage::Storage};
use serde::{Deserialize, Serialize};
use vibex_nsh_ui::copy::{self, LanguagePreference};

/// The storage key the app's preferences live under.
const SETTINGS_KEY: &str = "vibex.mobile.settings.v1";

/// Whether the hand wants feedback; the service reads it from here.
static HAPTICS_ENABLED: AtomicBool = AtomicBool::new(true);

/// Whether an active turn or the terminal holds the display awake.
static KEEP_SCREEN_ON: AtomicBool = AtomicBool::new(false);

/// Plays `haptic` when the reader left feedback on.
pub fn haptic(haptic: Haptic) {
    if HAPTICS_ENABLED.load(Ordering::Relaxed) {
        nsh_core::element::haptic(haptic);
    }
}

fn default_haptics() -> bool {
    true
}

fn default_font_scale() -> u32 {
    100
}

/// Whether the reader asked the screen to stay awake during work.
pub fn keep_screen_on() -> bool {
    KEEP_SCREEN_ON.load(Ordering::Relaxed)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSettings {
    #[serde(default)]
    appearance: String,
    #[serde(default)]
    language: String,
    #[serde(default = "default_haptics")]
    haptics: bool,
    #[serde(default)]
    keep_screen_on: bool,
    #[serde(default = "default_font_scale")]
    font_scale: u32,
}

/// The preferences the app can act on today, as signals the screens read.
#[derive(Clone)]
pub struct SettingsState {
    /// The appearance the phone is pinned to, or `System`.
    pub appearance: Signal<Appearance>,
    /// The language the phone is pinned to, or `System`.
    pub language: Signal<LanguagePreference>,
    /// Whether the hand gets feedback for finished actions.
    pub haptics: Signal<bool>,
    /// Whether a running turn or the terminal holds the display awake.
    pub keep_screen_on: Signal<bool>,
    /// The text-size step, as a percentage of the ladder.
    pub font_scale: Signal<u32>,
    /// Bumped on every language change: the root rebuilds the copy in place.
    pub revision: Signal<u64>,
}

impl SettingsState {
    /// Reads the stored preferences and publishes them.
    ///
    /// A phone that ran the GPUI client has its appearance and language in
    /// `app-settings.json`; when this app has no settings yet, that file is
    /// read once and written into the new store.
    pub fn new(data_dir: &Path) -> Self {
        let stored_text = Storage::get_item(SETTINGS_KEY);
        let imported = stored_text.is_none();
        let stored =
            match stored_text.and_then(|text| serde_json::from_str::<StoredSettings>(&text).ok()) {
                Some(stored) => stored,
                None => crate::migration::import_settings(data_dir)
                    .map(StoredSettings::from)
                    .unwrap_or_default(),
            };
        let state = Self {
            appearance: signal(appearance_from_storage(&stored.appearance)),
            language: signal(LanguagePreference::from_storage(&stored.language)),
            haptics: signal(stored.haptics),
            keep_screen_on: signal(stored.keep_screen_on),
            font_scale: signal(stored.font_scale),
            revision: signal(0),
        };
        if imported {
            state.persist();
        }
        state
    }

    /// Puts the stored preferences into effect. Called once, before the first
    /// window is drawn so the first frame already has the chosen language and
    /// palette.
    pub fn apply(&self) {
        nsh_core::element::set_color_scheme(self.appearance.get_untracked());
        copy::set_preference(self.language.get_untracked());
        HAPTICS_ENABLED.store(self.haptics.get_untracked(), Ordering::Relaxed);
        KEEP_SCREEN_ON.store(self.keep_screen_on.get_untracked(), Ordering::Relaxed);
        vibex_nsh_ui::theme::set_font_scale_percent(self.font_scale.get_untracked());
    }

    /// Chooses an appearance and keeps it.
    pub fn set_appearance(&self, appearance: Appearance) {
        if self.appearance.get_untracked() == appearance {
            return;
        }
        self.appearance.set(appearance);
        nsh_core::element::set_color_scheme(appearance);
        self.persist();
    }

    /// Chooses a language and keeps it.
    pub fn set_language(&self, language: LanguagePreference) {
        if self.language.get_untracked() == language {
            return;
        }
        self.language.set(language);
        copy::set_preference(language);
        self.revision
            .update(|revision| *revision = revision.wrapping_add(1));
        self.persist();
    }

    /// Chooses whether finished actions play the device's feedback.
    pub fn set_haptics(&self, enabled: bool) {
        if self.haptics.get_untracked() == enabled {
            return;
        }
        self.haptics.set(enabled);
        HAPTICS_ENABLED.store(enabled, Ordering::Relaxed);
        self.persist();
    }

    /// Chooses whether a running turn or the terminal holds the screen awake.
    pub fn set_keep_screen_on(&self, enabled: bool) {
        if self.keep_screen_on.get_untracked() == enabled {
            return;
        }
        self.keep_screen_on.set(enabled);
        KEEP_SCREEN_ON.store(enabled, Ordering::Relaxed);
        self.persist();
    }

    /// Chooses a text-size step and keeps it.
    pub fn set_font_scale(&self, percent: u32) {
        if self.font_scale.get_untracked() == percent {
            return;
        }
        self.font_scale.set(percent);
        vibex_nsh_ui::theme::set_font_scale_percent(percent);
        // Type is read while the tree is built, so a step change rebuilds the
        // surface the same way a language change does.
        self.revision
            .update(|revision| *revision = revision.wrapping_add(1));
        self.persist();
    }

    fn persist(&self) {
        let stored = StoredSettings {
            appearance: appearance_to_storage(self.appearance.get_untracked()).to_owned(),
            language: self.language.get_untracked().to_storage().to_owned(),
            haptics: self.haptics.get_untracked(),
            keep_screen_on: self.keep_screen_on.get_untracked(),
            font_scale: self.font_scale.get_untracked(),
        };
        if let Ok(text) = serde_json::to_string(&stored) {
            let _ = Storage::set_item(SETTINGS_KEY, text);
        }
    }
}

impl From<crate::migration::ImportedSettings> for StoredSettings {
    fn from(settings: crate::migration::ImportedSettings) -> Self {
        Self {
            appearance: settings.theme.unwrap_or_default(),
            language: settings.language.unwrap_or_default(),
            haptics: settings.haptics.unwrap_or_else(default_haptics),
            keep_screen_on: settings.keep_screen_on.unwrap_or_default(),
            font_scale: settings
                .font_scale_percent
                .unwrap_or_else(default_font_scale),
        }
    }
}

fn appearance_from_storage(value: &str) -> Appearance {
    match value {
        "light" => Appearance::Light,
        "dark" => Appearance::Dark,
        _ => Appearance::System,
    }
}

fn appearance_to_storage(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::System => "system",
        Appearance::Light => "light",
        Appearance::Dark => "dark",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_names_round_trip() {
        for appearance in [Appearance::System, Appearance::Light, Appearance::Dark] {
            assert_eq!(
                appearance_from_storage(appearance_to_storage(appearance)),
                appearance
            );
        }
    }
}

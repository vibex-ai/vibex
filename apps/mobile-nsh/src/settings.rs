//! The app's device preferences.
//!
//! The GPUI client keeps `app-settings.json`; the `nsh` client keeps the same
//! small set in the engine's key-value storage, so a theme or a language chosen
//! on the phone survives a restart. Appearance is applied through `nsh`'s
//! color-scheme command, and language through the copy resolver both clients
//! share.

use std::sync::atomic::{AtomicBool, Ordering};

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
    /// Bumped on every language change: the root rebuilds the copy in place.
    pub revision: Signal<u64>,
}

impl SettingsState {
    /// Reads the stored preferences and publishes them.
    pub fn new() -> Self {
        let stored = Storage::get_item(SETTINGS_KEY)
            .and_then(|text| serde_json::from_str::<StoredSettings>(&text).ok())
            .unwrap_or_default();
        Self {
            appearance: signal(appearance_from_storage(&stored.appearance)),
            language: signal(LanguagePreference::from_storage(&stored.language)),
            haptics: signal(stored.haptics),
            keep_screen_on: signal(stored.keep_screen_on),
            revision: signal(0),
        }
    }

    /// Puts the stored preferences into effect. Called once, before the first
    /// window is drawn so the first frame already has the chosen language and
    /// palette.
    pub fn apply(&self) {
        nsh_core::element::set_color_scheme(self.appearance.get_untracked());
        copy::set_preference(self.language.get_untracked());
        HAPTICS_ENABLED.store(self.haptics.get_untracked(), Ordering::Relaxed);
        KEEP_SCREEN_ON.store(self.keep_screen_on.get_untracked(), Ordering::Relaxed);
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

    fn persist(&self) {
        let stored = StoredSettings {
            appearance: appearance_to_storage(self.appearance.get_untracked()).to_owned(),
            language: self.language.get_untracked().to_storage().to_owned(),
            haptics: self.haptics.get_untracked(),
            keep_screen_on: self.keep_screen_on.get_untracked(),
        };
        if let Ok(text) = serde_json::to_string(&stored) {
            let _ = Storage::set_item(SETTINGS_KEY, text);
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

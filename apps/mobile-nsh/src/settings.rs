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

/// Whether the composer shows the session's token use.
static SHOW_TOKEN_USAGE: AtomicBool = AtomicBool::new(true);
/// Whether the composer shows the plan and goal bars.
static SHOW_PLAN_GOAL: AtomicBool = AtomicBool::new(true);
/// Whether the transcript shows the live generation row.
static SHOW_GENERATION_STATUS: AtomicBool = AtomicBool::new(true);
/// Whether the transcript shows the Agent's reasoning rows.
static SHOW_REASONING: AtomicBool = AtomicBool::new(true);
/// Whether a paused turn continues on its own.
static AUTO_CONTINUE: AtomicBool = AtomicBool::new(false);

/// The notification categories the reader wants; master, completed, needs
/// input and failure are bits of one byte, read on the event path.
static NOTIFICATION_POLICY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0b1111);

/// Which notification a turn produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationKind {
    Completed,
    NeedsInput,
    Failed,
}

/// Whether the reader asked for this category.
pub fn notification_allowed(kind: NotificationKind) -> bool {
    let policy = NOTIFICATION_POLICY.load(Ordering::Relaxed);
    let category = match kind {
        NotificationKind::Completed => 0b0010,
        NotificationKind::NeedsInput => 0b0100,
        NotificationKind::Failed => 0b1000,
    };
    policy & 0b0001 != 0 && policy & category != 0
}

fn notification_policy(
    notifications: bool,
    completed: bool,
    needs_input: bool,
    failed: bool,
) -> u8 {
    u8::from(notifications)
        | (u8::from(completed) << 1)
        | (u8::from(needs_input) << 2)
        | (u8::from(failed) << 3)
}

pub fn show_token_usage() -> bool {
    SHOW_TOKEN_USAGE.load(Ordering::Relaxed)
}

pub fn show_plan_goal() -> bool {
    SHOW_PLAN_GOAL.load(Ordering::Relaxed)
}

pub fn show_generation_status() -> bool {
    SHOW_GENERATION_STATUS.load(Ordering::Relaxed)
}

pub fn show_reasoning() -> bool {
    SHOW_REASONING.load(Ordering::Relaxed)
}

pub fn auto_continue() -> bool {
    AUTO_CONTINUE.load(Ordering::Relaxed)
}

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

fn default_true() -> bool {
    true
}

/// Whether the reader asked the screen to stay awake during work.
pub fn keep_screen_on() -> bool {
    KEEP_SCREEN_ON.load(Ordering::Relaxed)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    #[serde(default)]
    reduce_motion: bool,
    #[serde(default = "default_true")]
    notifications: bool,
    #[serde(default = "default_true")]
    notify_completed: bool,
    #[serde(default = "default_true")]
    notify_needs_input: bool,
    #[serde(default = "default_true")]
    notify_failed: bool,
    #[serde(default = "default_true")]
    show_token_usage: bool,
    #[serde(default = "default_true")]
    show_plan_goal: bool,
    #[serde(default = "default_true")]
    show_generation_status: bool,
    #[serde(default = "default_true")]
    show_reasoning: bool,
    #[serde(default)]
    auto_continue: bool,
}

impl Default for StoredSettings {
    fn default() -> Self {
        Self {
            appearance: String::new(),
            language: String::new(),
            haptics: default_haptics(),
            keep_screen_on: false,
            font_scale: default_font_scale(),
            reduce_motion: false,
            notifications: true,
            notify_completed: true,
            notify_needs_input: true,
            notify_failed: true,
            show_token_usage: true,
            show_plan_goal: true,
            show_generation_status: true,
            show_reasoning: true,
            auto_continue: false,
        }
    }
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
    /// Whether the app skips its own motion.
    pub reduce_motion: Signal<bool>,
    /// The notification master switch and its three categories.
    pub notifications: Signal<bool>,
    pub notify_completed: Signal<bool>,
    pub notify_needs_input: Signal<bool>,
    pub notify_failed: Signal<bool>,
    /// Whether the composer and transcript show their optional chrome.
    pub show_token_usage: Signal<bool>,
    pub show_plan_goal: Signal<bool>,
    pub show_generation_status: Signal<bool>,
    pub show_reasoning: Signal<bool>,
    pub auto_continue: Signal<bool>,
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
            font_scale: signal(normalized_font_scale(stored.font_scale)),
            reduce_motion: signal(stored.reduce_motion),
            notifications: signal(stored.notifications),
            notify_completed: signal(stored.notify_completed),
            notify_needs_input: signal(stored.notify_needs_input),
            notify_failed: signal(stored.notify_failed),
            show_token_usage: signal(stored.show_token_usage),
            show_plan_goal: signal(stored.show_plan_goal),
            show_generation_status: signal(stored.show_generation_status),
            show_reasoning: signal(stored.show_reasoning),
            auto_continue: signal(stored.auto_continue),
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
        vibex_nsh_ui::motion::set_haptics(self.haptics.get_untracked());
        KEEP_SCREEN_ON.store(self.keep_screen_on.get_untracked(), Ordering::Relaxed);
        SHOW_TOKEN_USAGE.store(self.show_token_usage.get_untracked(), Ordering::Relaxed);
        SHOW_PLAN_GOAL.store(self.show_plan_goal.get_untracked(), Ordering::Relaxed);
        SHOW_GENERATION_STATUS.store(
            self.show_generation_status.get_untracked(),
            Ordering::Relaxed,
        );
        vibex_nsh_ui::theme::set_font_scale_percent(self.font_scale.get_untracked());
        self.refresh_notifications();
        vibex_nsh_ui::motion::set_reduce_motion(self.reduce_motion.get_untracked());
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

    /// Chooses whether the app skips its own motion.
    pub fn set_reduce_motion(&self, reduce: bool) {
        if self.reduce_motion.get_untracked() == reduce {
            return;
        }
        self.reduce_motion.set(reduce);
        vibex_nsh_ui::motion::set_reduce_motion(reduce);
        self.persist();
    }

    /// Chooses whether finished actions play the device's feedback.
    pub fn set_haptics(&self, enabled: bool) {
        if self.haptics.get_untracked() == enabled {
            return;
        }
        self.haptics.set(enabled);
        HAPTICS_ENABLED.store(enabled, Ordering::Relaxed);
        vibex_nsh_ui::motion::set_haptics(enabled);
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
        let percent = normalized_font_scale(percent);
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

    /// Turns the notification master switch, or one of its categories, on or
    /// off.
    pub fn set_notifications(&self, enabled: bool) {
        self.notifications.set(enabled);
        self.refresh_notifications();
        self.persist();
    }

    pub fn set_notify_completed(&self, enabled: bool) {
        self.notify_completed.set(enabled);
        self.refresh_notifications();
        self.persist();
    }

    pub fn set_notify_needs_input(&self, enabled: bool) {
        self.notify_needs_input.set(enabled);
        self.refresh_notifications();
        self.persist();
    }

    pub fn set_show_token_usage(&self, enabled: bool) {
        self.show_token_usage.set(enabled);
        SHOW_TOKEN_USAGE.store(enabled, Ordering::Relaxed);
        self.persist();
    }

    pub fn set_show_plan_goal(&self, enabled: bool) {
        self.show_plan_goal.set(enabled);
        SHOW_PLAN_GOAL.store(enabled, Ordering::Relaxed);
        self.persist();
    }

    pub fn set_show_generation_status(&self, enabled: bool) {
        self.show_generation_status.set(enabled);
        SHOW_GENERATION_STATUS.store(enabled, Ordering::Relaxed);
        self.persist();
    }

    pub fn set_show_reasoning(&self, enabled: bool) {
        self.show_reasoning.set(enabled);
        SHOW_REASONING.store(enabled, Ordering::Relaxed);
        self.persist();
    }

    pub fn set_auto_continue(&self, enabled: bool) {
        self.auto_continue.set(enabled);
        AUTO_CONTINUE.store(enabled, Ordering::Relaxed);
        self.persist();
    }

    pub fn set_notify_failed(&self, enabled: bool) {
        self.notify_failed.set(enabled);
        self.refresh_notifications();
        self.persist();
    }

    fn refresh_notifications(&self) {
        NOTIFICATION_POLICY.store(
            notification_policy(
                self.notifications.get_untracked(),
                self.notify_completed.get_untracked(),
                self.notify_needs_input.get_untracked(),
                self.notify_failed.get_untracked(),
            ),
            Ordering::Relaxed,
        );
    }

    fn persist(&self) {
        let stored = StoredSettings {
            appearance: appearance_to_storage(self.appearance.get_untracked()).to_owned(),
            language: self.language.get_untracked().to_storage().to_owned(),
            haptics: self.haptics.get_untracked(),
            keep_screen_on: self.keep_screen_on.get_untracked(),
            font_scale: self.font_scale.get_untracked(),
            reduce_motion: self.reduce_motion.get_untracked(),
            notifications: self.notifications.get_untracked(),
            notify_completed: self.notify_completed.get_untracked(),
            notify_needs_input: self.notify_needs_input.get_untracked(),
            notify_failed: self.notify_failed.get_untracked(),
            show_token_usage: self.show_token_usage.get_untracked(),
            show_plan_goal: self.show_plan_goal.get_untracked(),
            show_generation_status: self.show_generation_status.get_untracked(),
            show_reasoning: self.show_reasoning.get_untracked(),
            auto_continue: self.auto_continue.get_untracked(),
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
            reduce_motion: false,
            notifications: true,
            notify_completed: true,
            notify_needs_input: true,
            notify_failed: true,
            show_token_usage: true,
            show_plan_goal: true,
            show_generation_status: true,
            show_reasoning: true,
            auto_continue: false,
        }
    }
}

fn normalized_font_scale(percent: u32) -> u32 {
    *vibex_nsh_ui::theme::FONT_SCALE_STEPS
        .iter()
        .min_by_key(|step| step.abs_diff(percent))
        .unwrap_or(&100)
}

fn appearance_from_storage(value: &str) -> Appearance {
    match value {
        "light" => Appearance::Light,
        "dark" => Appearance::Dark,
        "system" => Appearance::System,
        // Nothing stored yet, or a value from a build that wrote none: the app
        // paints a light page, so it starts light. Following the system here
        // left a phone in dark mode drawing white status-bar glyphs over that
        // light page, since the bar reads the appearance and the page does not.
        _ => Appearance::Light,
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
    fn conversation_chrome_defaults_to_on() {
        let stored = StoredSettings::default();
        assert!(stored.show_token_usage);
        assert!(stored.show_plan_goal);
        assert!(stored.show_generation_status);
    }

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

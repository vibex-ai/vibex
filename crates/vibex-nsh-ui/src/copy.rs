//! The phone's copy in the three languages it ships.
//!
//! The GPUI client keeps `apps/mobile/src/locale.rs`, a table of the same
//! triples with an explicit preference stored on the device. This module keeps
//! the same call shape — `text("Scan", "扫码", "掃碼")` — so a screen moves over
//! one label at a time, and resolves the platform's locale the way `nsh`
//! reports it (`zh-Hans-CN`, `zh-Hant-TW`, `en-US`).
//!
//! The app's own preference (Settings → Language) is kept in a process-global
//! atomic: every lookup resolves through it, while `System` falls back to the
//! platform's locale. The preference is persisted by the app; this module only
//! answers with it.

use std::cell::Cell;

use nsh_core::prelude::*;

/// The languages the phone UI has copy for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    /// English, the fallback.
    English,
    /// Simplified Chinese.
    SimplifiedChinese,
    /// Traditional Chinese.
    TraditionalChinese,
}

/// What the reader chose on the Settings page.
///
/// `System` follows the platform; the other three pin the UI's language, the
/// same way the GPUI client's preference does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LanguagePreference {
    /// The platform's own locale.
    #[default]
    System,
    /// English.
    English,
    /// Simplified Chinese.
    SimplifiedChinese,
    /// Traditional Chinese.
    TraditionalChinese,
}

impl LanguagePreference {
    /// Reads the stored preference, `System` for anything unknown.
    pub fn from_storage(value: &str) -> Self {
        match value {
            "en" => Self::English,
            "zh-cn" => Self::SimplifiedChinese,
            "zh-tw" => Self::TraditionalChinese,
            _ => Self::System,
        }
    }

    /// The value written to the settings file.
    pub fn to_storage(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => "en",
            Self::SimplifiedChinese => "zh-cn",
            Self::TraditionalChinese => "zh-tw",
        }
    }

    /// The label the Settings page shows for this choice.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
            Self::TraditionalChinese => "繁體中文",
        }
    }
}

const PREFERENCE_SYSTEM: u8 = 0;
const PREFERENCE_ENGLISH: u8 = 1;
const PREFERENCE_SIMPLIFIED: u8 = 2;
const PREFERENCE_TRADITIONAL: u8 = 3;

thread_local! {
    /// The app's preference on the thread that draws and handles the UI.
    /// One device runs one app on one engine thread, while tests run several
    /// headless apps side by side on their own threads.
    static PREFERENCE: Cell<u8> = const { Cell::new(PREFERENCE_SYSTEM) };
}

/// Applies the app's language preference for this process.
pub fn set_preference(preference: LanguagePreference) {
    PREFERENCE.with(|cell| {
        cell.set(match preference {
            LanguagePreference::System => PREFERENCE_SYSTEM,
            LanguagePreference::English => PREFERENCE_ENGLISH,
            LanguagePreference::SimplifiedChinese => PREFERENCE_SIMPLIFIED,
            LanguagePreference::TraditionalChinese => PREFERENCE_TRADITIONAL,
        });
    });
}

/// The app's language preference, as the Settings page reads it.
pub fn preference() -> LanguagePreference {
    PREFERENCE.with(|cell| match cell.get() {
        PREFERENCE_ENGLISH => LanguagePreference::English,
        PREFERENCE_SIMPLIFIED => LanguagePreference::SimplifiedChinese,
        PREFERENCE_TRADITIONAL => LanguagePreference::TraditionalChinese,
        _ => LanguagePreference::System,
    })
}

/// The language the UI currently draws in.
///
/// The app's choice wins; `System` resolves the platform's locale. The region
/// decides Simplified against Traditional, as the GPUI client does.
pub fn language() -> Language {
    match preference() {
        LanguagePreference::English => Language::English,
        LanguagePreference::SimplifiedChinese => Language::SimplifiedChinese,
        LanguagePreference::TraditionalChinese => Language::TraditionalChinese,
        LanguagePreference::System => {
            let tag = locale();
            language_for_tag(tag.as_str())
        }
    }
}

fn language_for_tag(tag: &str) -> Language {
    if !tag.starts_with("zh") {
        return Language::English;
    }
    let traditional =
        tag.contains("-Hant") || tag.contains("-TW") || tag.contains("-HK") || tag.contains("-MO");
    if traditional {
        Language::TraditionalChinese
    } else {
        Language::SimplifiedChinese
    }
}

/// The copy for the current language: English, Simplified Chinese, Traditional
/// Chinese, in that order.
pub fn text(
    english: &'static str,
    simplified_chinese: &'static str,
    traditional_chinese: &'static str,
) -> &'static str {
    match language() {
        Language::English => english,
        Language::SimplifiedChinese => simplified_chinese,
        Language::TraditionalChinese => traditional_chinese,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_preference_round_trips() {
        for preference in [
            LanguagePreference::System,
            LanguagePreference::English,
            LanguagePreference::SimplifiedChinese,
            LanguagePreference::TraditionalChinese,
        ] {
            assert_eq!(
                LanguagePreference::from_storage(preference.to_storage()),
                preference
            );
        }
    }

    #[test]
    fn an_explicit_preference_wins_over_the_platform() {
        let previous = preference();
        set_preference(LanguagePreference::English);
        assert_eq!(language(), Language::English);
        set_preference(LanguagePreference::TraditionalChinese);
        assert_eq!(language(), Language::TraditionalChinese);
        set_preference(LanguagePreference::SimplifiedChinese);
        assert_eq!(language(), Language::SimplifiedChinese);
        set_preference(previous);
    }
}

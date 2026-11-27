//! The phone's copy in every language Vibex ships.
//!
//! The GPUI clients keep a larger locale table in `vibex-locale`; this module
//! keeps the phone's call shape — `text("Scan", "扫码", "掃碼")` — so a screen
//! spells one sentence in the languages its layout already knew. The seven
//! languages added beside English and Chinese are keyed by that English
//! sentence in the shared table, so a translation reaches the phone without a
//! second copy of the wording.
//!
//! The app's own preference (Settings → Language) is kept in a process-global
//! atomic: every lookup resolves through it, while `System` follows the
//! platform's locale. The preference is persisted by the app; this module only
//! answers with it.

use std::cell::Cell;

use nsh_core::prelude::*;
use vibex_locale::{locale::Locale, locale_copy};

/// The languages the phone UI draws in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    English,
    SimplifiedChinese,
    TraditionalChinese,
    Japanese,
    Korean,
    German,
    French,
    Spanish,
    BrazilianPortuguese,
    Russian,
}

impl Language {
    /// The shared locale this language draws from.
    pub fn locale(self) -> Locale {
        match self {
            Self::English => Locale::En,
            Self::SimplifiedChinese => Locale::ZhCn,
            Self::TraditionalChinese => Locale::ZhTw,
            Self::Japanese => Locale::Ja,
            Self::Korean => Locale::Ko,
            Self::German => Locale::De,
            Self::French => Locale::Fr,
            Self::Spanish => Locale::Es,
            Self::BrazilianPortuguese => Locale::PtBr,
            Self::Russian => Locale::Ru,
        }
    }
}

/// What the reader chose on the Settings page.
///
/// `System` follows the platform; every other choice pins the UI's language,
/// the same way the desktop client's preference does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LanguagePreference {
    /// The platform's own locale.
    #[default]
    System,
    English,
    SimplifiedChinese,
    TraditionalChinese,
    Japanese,
    Korean,
    German,
    French,
    Spanish,
    BrazilianPortuguese,
    Russian,
}

impl LanguagePreference {
    /// The language this choice draws in, when it is not `System`.
    fn language(self) -> Option<Language> {
        match self {
            Self::System => None,
            Self::English => Some(Language::English),
            Self::SimplifiedChinese => Some(Language::SimplifiedChinese),
            Self::TraditionalChinese => Some(Language::TraditionalChinese),
            Self::Japanese => Some(Language::Japanese),
            Self::Korean => Some(Language::Korean),
            Self::German => Some(Language::German),
            Self::French => Some(Language::French),
            Self::Spanish => Some(Language::Spanish),
            Self::BrazilianPortuguese => Some(Language::BrazilianPortuguese),
            Self::Russian => Some(Language::Russian),
        }
    }

    /// Reads the stored preference, `System` for anything unknown.
    pub fn from_storage(value: &str) -> Self {
        match value {
            "en" => Self::English,
            "zh-cn" => Self::SimplifiedChinese,
            "zh-tw" => Self::TraditionalChinese,
            "ja" => Self::Japanese,
            "ko" => Self::Korean,
            "de" => Self::German,
            "fr" => Self::French,
            "es" => Self::Spanish,
            "pt-br" => Self::BrazilianPortuguese,
            "ru" => Self::Russian,
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
            Self::Japanese => "ja",
            Self::Korean => "ko",
            Self::German => "de",
            Self::French => "fr",
            Self::Spanish => "es",
            Self::BrazilianPortuguese => "pt-br",
            Self::Russian => "ru",
        }
    }

    /// The label the Settings page shows for this choice.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
            Self::TraditionalChinese => "繁體中文",
            Self::Japanese => "日本語",
            Self::Korean => "한국어",
            Self::German => "Deutsch",
            Self::French => "Français",
            Self::Spanish => "Español",
            Self::BrazilianPortuguese => "Português (BR)",
            Self::Russian => "Русский",
        }
    }
}

const PREFERENCE_SYSTEM: u8 = 0;
const PREFERENCE_ENGLISH: u8 = 1;
const PREFERENCE_SIMPLIFIED: u8 = 2;
const PREFERENCE_TRADITIONAL: u8 = 3;
const PREFERENCE_JAPANESE: u8 = 4;
const PREFERENCE_KOREAN: u8 = 5;
const PREFERENCE_GERMAN: u8 = 6;
const PREFERENCE_FRENCH: u8 = 7;
const PREFERENCE_SPANISH: u8 = 8;
const PREFERENCE_PORTUGUESE: u8 = 9;
const PREFERENCE_RUSSIAN: u8 = 10;

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
            LanguagePreference::Japanese => PREFERENCE_JAPANESE,
            LanguagePreference::Korean => PREFERENCE_KOREAN,
            LanguagePreference::German => PREFERENCE_GERMAN,
            LanguagePreference::French => PREFERENCE_FRENCH,
            LanguagePreference::Spanish => PREFERENCE_SPANISH,
            LanguagePreference::BrazilianPortuguese => PREFERENCE_PORTUGUESE,
            LanguagePreference::Russian => PREFERENCE_RUSSIAN,
        });
    });
}

/// The app's language preference, as the Settings page reads it.
pub fn preference() -> LanguagePreference {
    PREFERENCE.with(|cell| match cell.get() {
        PREFERENCE_ENGLISH => LanguagePreference::English,
        PREFERENCE_SIMPLIFIED => LanguagePreference::SimplifiedChinese,
        PREFERENCE_TRADITIONAL => LanguagePreference::TraditionalChinese,
        PREFERENCE_JAPANESE => LanguagePreference::Japanese,
        PREFERENCE_KOREAN => LanguagePreference::Korean,
        PREFERENCE_GERMAN => LanguagePreference::German,
        PREFERENCE_FRENCH => LanguagePreference::French,
        PREFERENCE_SPANISH => LanguagePreference::Spanish,
        PREFERENCE_PORTUGUESE => LanguagePreference::BrazilianPortuguese,
        PREFERENCE_RUSSIAN => LanguagePreference::Russian,
        _ => LanguagePreference::System,
    })
}

/// The language the UI currently draws in.
///
/// The app's choice wins; `System` resolves the platform's locale.
pub fn language() -> Language {
    preference().language().unwrap_or_else(|| {
        let tag = locale();
        language_for_tag(tag.as_str())
    })
}

fn language_for_tag(tag: &str) -> Language {
    match Locale::from_system_tag(Some(tag)) {
        Locale::En => Language::English,
        Locale::ZhCn => Language::SimplifiedChinese,
        Locale::ZhTw => Language::TraditionalChinese,
        Locale::Ja => Language::Japanese,
        Locale::Ko => Language::Korean,
        Locale::De => Language::German,
        Locale::Fr => Language::French,
        Locale::Es => Language::Spanish,
        Locale::PtBr => Language::BrazilianPortuguese,
        Locale::Ru => Language::Russian,
    }
}

/// The copy for the current language: the English source, the two Chinese
/// strings this call site already spells, or the shared translation table for
/// every other language.
pub fn text(
    english: &'static str,
    simplified_chinese: &'static str,
    traditional_chinese: &'static str,
) -> &'static str {
    match language() {
        Language::English => english,
        Language::SimplifiedChinese => simplified_chinese,
        Language::TraditionalChinese => traditional_chinese,
        other => locale_copy::localized(other.locale(), english),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [LanguagePreference; 11] = [
        LanguagePreference::System,
        LanguagePreference::English,
        LanguagePreference::SimplifiedChinese,
        LanguagePreference::TraditionalChinese,
        LanguagePreference::Japanese,
        LanguagePreference::Korean,
        LanguagePreference::German,
        LanguagePreference::French,
        LanguagePreference::Spanish,
        LanguagePreference::BrazilianPortuguese,
        LanguagePreference::Russian,
    ];

    #[test]
    fn every_stored_preference_round_trips() {
        for preference in ALL {
            assert_eq!(
                LanguagePreference::from_storage(preference.to_storage()),
                preference
            );
        }
    }

    #[test]
    fn an_explicit_preference_wins_over_the_platform() {
        let previous = preference();
        for preference in ALL.into_iter().filter(|p| *p != LanguagePreference::System) {
            set_preference(preference);
            assert_eq!(language(), preference.language().unwrap());
        }
        set_preference(previous);
    }

    #[test]
    fn the_platform_resolves_every_shipped_language() {
        assert_eq!(language_for_tag("en-US"), Language::English);
        assert_eq!(language_for_tag("zh-Hans-CN"), Language::SimplifiedChinese);
        assert_eq!(language_for_tag("zh-Hant-TW"), Language::TraditionalChinese);
        assert_eq!(language_for_tag("ja-JP"), Language::Japanese);
        assert_eq!(language_for_tag("ko-KR"), Language::Korean);
        assert_eq!(language_for_tag("de-DE"), Language::German);
        assert_eq!(language_for_tag("fr-FR"), Language::French);
        assert_eq!(language_for_tag("es-ES"), Language::Spanish);
        assert_eq!(language_for_tag("pt-BR"), Language::BrazilianPortuguese);
        assert_eq!(language_for_tag("ru-RU"), Language::Russian);
    }

    #[test]
    fn a_new_language_uses_the_shared_table() {
        set_preference(LanguagePreference::Japanese);
        // This sentence is in the shared desktop table.
        assert_eq!(text("Remove", "移除", "移除"), "削除");
        set_preference(LanguagePreference::English);
    }
}

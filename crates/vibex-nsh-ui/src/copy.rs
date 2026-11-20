//! The phone's copy in the three languages it ships.
//!
//! The GPUI client keeps `apps/mobile/src/locale.rs`, a table of the same
//! triples with an explicit preference stored on the device. This module keeps
//! the same call shape — `text("Scan", "扫码", "掃碼")` — so a screen moves over
//! one label at a time, and resolves the platform's locale the way `nsh`
//! reports it (`zh-Hans-CN`, `zh-Hant-TW`, `en-US`). A user preference stored
//! by the app itself will feed the same resolver when settings move over.

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

/// The language of the platform's locale.
pub fn language() -> Language {
    let tag = locale();
    let tag = tag.as_str();
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

//! Platform-neutral locale classification shared by the native clients.
//!
//! The clients deliberately support only the languages for which Vibex ships
//! complete product copy.  Any other system language falls back to English.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    ZhCn,
    ZhTw,
}

impl Locale {
    pub const fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
            Self::ZhTw => "zh-TW",
        }
    }

    /// Resolve a BCP-47 or POSIX-style locale identifier.
    ///
    /// Chinese script/region variants are kept distinct; all non-Chinese
    /// languages intentionally use the English product copy.
    pub fn from_system_tag(tag: Option<&str>) -> Self {
        let tag = tag
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .replace('_', "-");
        if tag.starts_with("zh-tw")
            || tag.starts_with("zh-hk")
            || tag.starts_with("zh-mo")
            || tag == "yue"
            || tag.starts_with("yue-")
            || tag.split('-').any(|part| part == "hant")
        {
            Self::ZhTw
        } else if tag == "zh" || tag.starts_with("zh-") || tag == "cmn" || tag.starts_with("cmn-") {
            Self::ZhCn
        } else {
            Self::En
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Locale;

    #[test]
    fn resolves_supported_and_unknown_system_languages() {
        assert_eq!(Locale::from_system_tag(Some("zh_CN.UTF-8")), Locale::ZhCn);
        assert_eq!(Locale::from_system_tag(Some("zh-Hans-CN")), Locale::ZhCn);
        assert_eq!(Locale::from_system_tag(Some("zh-TW")), Locale::ZhTw);
        assert_eq!(Locale::from_system_tag(Some("zh-Hant")), Locale::ZhTw);
        assert_eq!(Locale::from_system_tag(Some("zh-HK")), Locale::ZhTw);
        assert_eq!(Locale::from_system_tag(Some("yue-Hant-HK")), Locale::ZhTw);
        assert_eq!(Locale::from_system_tag(Some("cmn-Hans-CN")), Locale::ZhCn);
        assert_eq!(Locale::from_system_tag(Some("en-US")), Locale::En);
        assert_eq!(Locale::from_system_tag(Some("fr-FR")), Locale::En);
        assert_eq!(Locale::from_system_tag(None), Locale::En);
    }
}

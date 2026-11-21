//! Platform-neutral locale classification shared by the native clients.
//!
//! The clients deliberately support only the languages for which Vibex ships
//! complete product copy.  Any other system language falls back to English.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    En,
    ZhCn,
    ZhTw,
    Ja,
    Ko,
    De,
    Fr,
    Es,
    PtBr,
    Ru,
}

impl Locale {
    /// Select product copy without introducing a client-global locale dependency.
    /// `en` in this locale.
    ///
    /// The two Chinese arguments stay in the signature because every call site
    /// spells its sentence in all three shipped languages. The seven languages
    /// added later are keyed by the English sentence in
    /// [`crate::locale_copy`], so no call site has to name them.
    pub fn text(self, en: &'static str, zh_cn: &'static str, zh_tw: &'static str) -> &'static str {
        match self {
            Self::En => en,
            Self::ZhCn => zh_cn,
            Self::ZhTw => zh_tw,
            other => crate::locale_copy::localized(other, en),
        }
    }

    /// `en` in this locale, or English when the copy table has no row for it.
    pub fn localized(self, en: &'static str) -> &'static str {
        match self {
            Self::En | Self::ZhCn | Self::ZhTw => en,
            other => crate::locale_copy::localized(other, en),
        }
    }

    pub const fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
            Self::ZhTw => "zh-TW",
            Self::Ja => "ja",
            Self::Ko => "ko",
            Self::De => "de",
            Self::Fr => "fr",
            Self::Es => "es",
            Self::PtBr => "pt-BR",
            Self::Ru => "ru",
        }
    }

    /// The three slots a new-session sentence is drawn from, in the order the
    /// reader's language puts them: the words before the Agent's mark, the
    /// connector, and the words after it.
    ///
    /// English keeps the phrase whole and ends on the mark — a sentence as
    /// ordinary as "start something new" has no room for a conjunction on its
    /// left. Chinese reads the connector first, which is what lands the mark
    /// between it and the words it governs. Every native client splits the
    /// sentence here, so the Agent's mark falls in the same place in all of
    /// them rather than each client deciding again what its language means.
    pub fn new_session_slogan(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::En => ("Start something new", "with", ""),
            Self::ZhCn => ("", "和", "开始新的工作"),
            Self::ZhTw => ("", "和", "開始新的工作"),
            Self::Ja => ("", "と ", "新しい作業を始める"),
            Self::Ko => ("", "와 함께 ", "새로운 작업 시작"),
            Self::De => ("Etwas Neues starten", "mit", ""),
            Self::Fr => ("Commencer quelque chose de nouveau", "avec", ""),
            Self::Es => ("Empieza algo nuevo", "con", ""),
            Self::PtBr => ("Comece algo novo", "com", ""),
            Self::Ru => ("Начните новое", "с", ""),
        }
    }

    /// The new-session sentence with the Agent's name where its mark stands.
    ///
    /// The mark is the one part of the line a screen reader cannot read, so the
    /// sentence is spoken with the name in its place. A page with no Agent
    /// chosen yet has nothing to put there and says the plain sentence instead.
    pub fn new_session_slogan_spoken(self, agent: Option<&str>) -> String {
        let (lead, connector, trail) = self.new_session_slogan();
        let agent = agent.map(str::trim).filter(|agent| !agent.is_empty());
        match agent {
            Some(agent) => [lead, connector, agent, trail]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            None => [lead, connector, trail]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
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
        } else if tag.starts_with("ja") {
            Self::Ja
        } else if tag.starts_with("ko") {
            Self::Ko
        } else if tag.starts_with("de") {
            Self::De
        } else if tag.starts_with("fr") {
            Self::Fr
        } else if tag.starts_with("es") {
            Self::Es
        } else if tag.starts_with("pt") {
            // Brazilian Portuguese is the only Portuguese Vibex ships; a
            // European tag resolves here too rather than falling back to
            // English, which would be a language the reader did not pick.
            Self::PtBr
        } else if tag.starts_with("ru") {
            Self::Ru
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
        assert_eq!(Locale::from_system_tag(None), Locale::En);
        // A language no client ships still falls back to English.
        assert_eq!(Locale::from_system_tag(Some("th-TH")), Locale::En);
        assert_eq!(Locale::from_system_tag(Some("xx-YY")), Locale::En);
    }

    /// Every language the clients ship resolves from its own tag, including the
    /// POSIX uppercase form a Linux `LANG` uses.
    #[test]
    fn resolves_every_shipped_language() {
        for (tag, expected) in [
            ("en-US", Locale::En),
            ("zh-CN", Locale::ZhCn),
            ("zh-TW", Locale::ZhTw),
            ("ja", Locale::Ja),
            ("ja_JP.UTF-8", Locale::Ja),
            ("ko-KR", Locale::Ko),
            ("de", Locale::De),
            ("de_DE.UTF-8", Locale::De),
            ("fr-FR", Locale::Fr),
            ("es-ES", Locale::Es),
            ("pt-BR", Locale::PtBr),
            ("pt_PT.UTF-8", Locale::PtBr),
            ("ru-RU", Locale::Ru),
        ] {
            assert_eq!(Locale::from_system_tag(Some(tag)), expected, "{tag}");
        }
    }

    /// The added languages translate through the shared catalogue, and a string
    /// the catalogue does not carry stays English rather than going blank.
    #[test]
    fn the_added_languages_read_the_catalogue_and_fall_back_to_english() {
        assert_eq!(Locale::Ja.text("Settings", "设置", "設定"), "設定");
        assert_eq!(Locale::De.text("Cancel", "取消", "取消"), "Abbrechen");
        assert_eq!(
            Locale::Ru.text("Retry", "重试", "重試"),
            "Повторить попытку"
        );
        // The three shipped languages keep their own arguments.
        assert_eq!(Locale::ZhCn.text("Settings", "设置", "設定"), "设置");
        assert_eq!(Locale::En.text("Settings", "设置", "設定"), "Settings");
        // A sentence with no row is shown in English, not dropped.
        let missing = "a sentence the catalogue has never seen";
        assert_eq!(Locale::Ja.text(missing, "", ""), missing);
    }

    /// The sentence is split by the client's language only, so the shared
    /// splitter is what puts the Agent's mark in the same place in every
    /// client. Chinese reads its connector first and English reads its phrase
    /// whole, and the spoken form is the sentence with the mark's Agent named.
    #[test]
    fn the_new_session_sentence_is_split_and_spoken_the_same_way() {
        assert_eq!(
            Locale::En.new_session_slogan(),
            ("Start something new", "with", "")
        );
        assert_eq!(
            Locale::ZhCn.new_session_slogan(),
            ("", "和", "开始新的工作")
        );
        assert_eq!(
            Locale::ZhTw.new_session_slogan(),
            ("", "和", "開始新的工作")
        );

        assert_eq!(
            Locale::En.new_session_slogan_spoken(Some("Codex")),
            "Start something new with Codex"
        );
        assert_eq!(
            Locale::ZhCn.new_session_slogan_spoken(Some("Codex")),
            "和 Codex 开始新的工作"
        );
        // A page with no Agent chosen yet says the sentence alone rather than
        // leaving a hole where the mark would stand.
        assert_eq!(
            Locale::ZhCn.new_session_slogan_spoken(None),
            "和 开始新的工作"
        );
        assert_eq!(
            Locale::En.new_session_slogan_spoken(Some("   ")),
            "Start something new with"
        );
    }
}

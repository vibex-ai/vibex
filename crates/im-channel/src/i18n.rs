#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImLocale {
    En,
    ZhCn,
    ZhTw,
}

impl ImLocale {
    pub fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or_default() {
            value if value.starts_with("zh-TW") || value.starts_with("zh-Hant") => Self::ZhTw,
            value if value.starts_with("zh") => Self::ZhCn,
            _ => Self::En,
        }
    }

    pub const fn text(
        self,
        en: &'static str,
        zh_cn: &'static str,
        zh_tw: &'static str,
    ) -> &'static str {
        match self {
            Self::En => en,
            Self::ZhCn => zh_cn,
            Self::ZhTw => zh_tw,
        }
    }
}

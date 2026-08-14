use std::borrow::Cow;

use gpui::{App, AssetSource, Result, SharedString};

const IBM_PLEX_SANS_REGULAR: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf");
const IBM_PLEX_SANS_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-Italic.ttf");
const IBM_PLEX_SANS_SEMIBOLD: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf");
const IBM_PLEX_SANS_SEMIBOLD_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBoldItalic.ttf");
const WQY_MICROHEI: &[u8] = include_bytes!("../assets/fonts/wqy-microhei/wqy-microhei.ttc");

const ICONS: &[&str] = &[
    "brand/logo.svg",
    "icons/menu.svg",
    "icons/plus.svg",
    "icons/refresh.svg",
    "icons/scan-line.svg",
    "icons/send.svg",
    "icons/stop.svg",
    "icons/x.svg",
];

pub struct MobileAssets;

pub fn load_fonts(cx: &mut App) -> Result<()> {
    cx.text_system().add_fonts(vec![
        Cow::Borrowed(IBM_PLEX_SANS_REGULAR),
        Cow::Borrowed(IBM_PLEX_SANS_ITALIC),
        Cow::Borrowed(IBM_PLEX_SANS_SEMIBOLD),
        Cow::Borrowed(IBM_PLEX_SANS_SEMIBOLD_ITALIC),
        Cow::Borrowed(WQY_MICROHEI),
    ])
}

impl AssetSource for MobileAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "brand/logo.svg" => Some(include_bytes!("../assets/brand/logo.svg")),
            "icons/menu.svg" => Some(include_bytes!("../assets/icons/menu.svg")),
            "icons/plus.svg" => Some(include_bytes!("../assets/icons/plus.svg")),
            "icons/refresh.svg" => Some(include_bytes!("../assets/icons/refresh.svg")),
            "icons/scan-line.svg" => Some(include_bytes!("../assets/icons/scan-line.svg")),
            "icons/send.svg" => Some(include_bytes!("../assets/icons/send.svg")),
            "icons/stop.svg" => Some(include_bytes!("../assets/icons/stop.svg")),
            "icons/x.svg" => Some(include_bytes!("../assets/icons/x.svg")),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|item| item.starts_with(path))
            .map(|item| SharedString::from(*item))
            .collect())
    }
}

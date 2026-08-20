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
    "icons/circle-plus.svg",
    "icons/search.svg",
    "icons/sliders-horizontal.svg",
    "icons/refresh.svg",
    "icons/scan-line.svg",
    "icons/send.svg",
    "icons/stop.svg",
    "icons/x.svg",
    "icons/settings.svg",
    "icons/activity.svg",
    "icons/server.svg",
    "icons/chevron-right.svg",
    "icons/chevron-left.svg",
    "icons/chevron-down.svg",
    "icons/message-square.svg",
    "icons/pin.svg",
    "icons/crosshair.svg",
    "icons/log-out.svg",
    "icons/ellipsis-vertical.svg",
    "icons/chevrons-right-left.svg",
    "icons/chevrons-left-right.svg",
    "icons/pencil.svg",
    "icons/file-archive.svg",
    "icons/trash-2.svg",
    "icons/openai.svg",
    "icons/claude.svg",
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
            "icons/circle-plus.svg" => Some(include_bytes!("../assets/icons/circle-plus.svg")),
            "icons/search.svg" => Some(include_bytes!("../assets/icons/search.svg")),
            "icons/sliders-horizontal.svg" => {
                Some(include_bytes!("../assets/icons/sliders-horizontal.svg"))
            }
            "icons/refresh.svg" => Some(include_bytes!("../assets/icons/refresh.svg")),
            "icons/scan-line.svg" => Some(include_bytes!("../assets/icons/scan-line.svg")),
            "icons/send.svg" => Some(include_bytes!("../assets/icons/send.svg")),
            "icons/stop.svg" => Some(include_bytes!("../assets/icons/stop.svg")),
            "icons/x.svg" => Some(include_bytes!("../assets/icons/x.svg")),
            "icons/settings.svg" => Some(include_bytes!("../assets/icons/settings.svg")),
            "icons/activity.svg" => Some(include_bytes!("../assets/icons/activity.svg")),
            "icons/server.svg" => Some(include_bytes!("../assets/icons/server.svg")),
            "icons/chevron-right.svg" => Some(include_bytes!("../assets/icons/chevron-right.svg")),
            "icons/chevron-left.svg" => Some(include_bytes!("../assets/icons/chevron-left.svg")),
            "icons/chevron-down.svg" => Some(include_bytes!("../assets/icons/chevron-down.svg")),
            "icons/message-square.svg" => {
                Some(include_bytes!("../assets/icons/message-square.svg"))
            }
            "icons/pin.svg" => Some(include_bytes!("../assets/icons/pin.svg")),
            "icons/crosshair.svg" => Some(include_bytes!("../assets/icons/crosshair.svg")),
            "icons/log-out.svg" => Some(include_bytes!("../assets/icons/log-out.svg")),
            "icons/ellipsis-vertical.svg" => {
                Some(include_bytes!("../assets/icons/ellipsis-vertical.svg"))
            }
            // The collapse/expand chevrons are the reviewed desktop marks so the
            // sessions toolbar reads identically on both shells.
            "icons/chevrons-right-left.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/chevrons-right-left.svg"
            )),
            "icons/chevrons-left-right.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/chevrons-left-right.svg"
            )),
            "icons/pencil.svg" => Some(include_bytes!("../../desktop/assets/icons/pencil.svg")),
            "icons/file-archive.svg" => Some(include_bytes!(
                "../../desktop/assets/icons/file-archive.svg"
            )),
            "icons/trash-2.svg" => Some(include_bytes!("../../desktop/assets/icons/trash-2.svg")),
            // Reuse the reviewed desktop provider marks so the compact client
            // and desktop sidebar show the same Agent identity.
            "icons/openai.svg" => Some(include_bytes!("../../desktop/assets/icons/openai.svg")),
            "icons/claude.svg" => Some(include_bytes!("../../desktop/assets/icons/claude.svg")),
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

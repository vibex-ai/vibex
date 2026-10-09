//! Authored raster material ramps. These colors belong to the game artwork;
//! surrounding controls and the preview's compositing surface use the UI theme.

use gpui::{App, Hsla, rgb};
use gpui_component::ActiveTheme as _;

use super::{guardian::Guardian, raster::ink};

const MATERIALS: [u32; ink::COUNT] = [
    0x000000, 0x242931, 0x353d44, 0x6c716a, 0x939580, 0xb0af95, 0x737771, 0x4c5660, 0x7a8182,
    0x354f49, 0x567655, 0x8fa16b, 0xbdc488, 0x3e4249, 0x5b625f, 0x858d7d, 0xb0b7a0, 0xd8d9b7,
    0xd3b774, 0x86744f, 0x623744, 0xcc5975, 0xf29c9c, 0xffefd2, 0xab7560, 0xe9b890, 0x312b38,
    0x554252, 0x2d4158, 0x4f7897, 0x8bb2bd, 0x9a4b5d, 0xd67976, 0x555e56, 0x254f60, 0x3a7787,
    0x8fc6bf, 0xe78963, 0xedb779, 0x9b724e, 0xbdb499, 0xe3dec0, 0xc49b67,
];

pub(super) fn material_colors(guardian: Guardian) -> [Hsla; ink::COUNT] {
    let mut colors = MATERIALS.map(|value| rgb(value).into());
    let body = match guardian {
        Guardian::Claude => [0x633f3a, 0x9b5143, 0xc76d50, 0xe18a65, 0xf1b58a],
        Guardian::Codex => [0x304743, 0x49675a, 0x72917a, 0xa3b79a, 0xd2d9b8],
        Guardian::Pi => [0x464150, 0x706476, 0xb6a48e, 0xe0d0aa, 0xf6eacb],
        Guardian::OpenCode => [0x344049, 0x51616a, 0x829298, 0xb9c7c5, 0xe2e6d5],
        Guardian::DeepSeek => [0x293955, 0x3b5482, 0x567daf, 0x87b3d0, 0xc1dbe0],
        Guardian::Copilot => [0x343b50, 0x57607a, 0x8990aa, 0xbcbcd0, 0xe7dbdd],
    };
    for (ix, value) in body.into_iter().enumerate() {
        colors[ink::BODY_SHADOW as usize + ix] = rgb(value).into();
    }
    colors
}

pub(super) fn colors(
    guardian: Guardian,
    preview: bool,
    reveal: f32,
    cx: &App,
) -> [Hsla; ink::COUNT] {
    let mut colors = material_colors(guardian);
    let background = cx.theme().background;
    if preview || reveal < 1.0 {
        let foreground = cx.theme().foreground;
        for (ix, color) in colors.iter_mut().enumerate() {
            let depth = match ix as u8 {
                ink::SHADOW => 0.055,
                ink::OUTLINE | ink::BODY_SHADOW | ink::BODY_DARK => 0.26,
                ink::CORE | ink::CORE_LIGHT | ink::WHITE => 0.30,
                _ => 0.19,
            };
            // Keep the silhouette readable in both themes, retaining a trace of
            // each material's hue instead of imposing a second page background.
            let idle = background
                .blend(foreground.opacity(depth))
                .blend(color.opacity(0.18));
            *color = if preview {
                idle
            } else {
                idle.blend(color.opacity(reveal))
            };
        }
    }
    colors
}

pub(super) fn ground_colors(guardian: Guardian, reveal: f32, cx: &App) -> [Hsla; ink::COUNT] {
    material_colors(guardian).map(|color| cx.theme().background.blend(color.opacity(reveal)))
}

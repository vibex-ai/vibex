//! Authored raster materials. Home and combat use these exact same colors;
//! only the map's presence changes during entry and departure.

use super::{guardian::Guardian, raster::ink};
use gpui::{Hsla, rgb};
const MATERIALS: [u32; ink::COUNT] = [
    0x000000, 0x19222c, 0x18222b, 0x55635b, 0x69776a, 0x82907c, 0x424d48, 0x35454c, 0x88978b,
    0x273d36, 0x425c43, 0x73845d, 0x98a477, 0x3e4249, 0x5b625f, 0x858d7d, 0xb0b7a0, 0xd8d9b7,
    0xd3b774, 0x86744f, 0x623744, 0xcc5975, 0xf29c9c, 0xffefd2, 0xab7560, 0xe9b890, 0x312b38,
    0x554252, 0x2d4158, 0x4f7897, 0x8bb2bd, 0x9a4b5d, 0xd67976, 0x1c2d32, 0x214751, 0x326572,
    0x85b9b9, 0xe78963, 0xedb779, 0x9b724e, 0xbdb499, 0xe3dec0, 0xc49b67,
];
pub(super) fn material_colors(guardian: Guardian) -> [Hsla; ink::COUNT] {
    let mut colors = MATERIALS.map(|value| rgb(value).into());
    let body = match guardian {
        Guardian::Claude => [0x582c3d, 0x993b38, 0xd55b39, 0xf7884d, 0xffbd76],
        Guardian::Codex => [0x2c2927, 0x564b3d, 0x8f7958, 0xc3aa79, 0xecdaa3],
        Guardian::Pi => [0x0c1119, 0x1a222a, 0x303a42, 0x4d5860, 0x747d80],
        Guardian::OpenCode => [0x141420, 0x292938, 0x444555, 0x676a78, 0x94979f],
        Guardian::DeepSeek => [0x142854, 0x253f8d, 0x385ac4, 0x5588e3, 0x89c6f4],
        Guardian::Copilot => [0x11393e, 0x155958, 0x228e80, 0x5ac8a4, 0xa3e8bd],
    };
    for (ix, value) in body.into_iter().enumerate() {
        colors[ink::BODY_SHADOW as usize + ix] = rgb(value).into();
    }
    let floor = match guardian {
        Guardian::Claude => [
            0x24322d, 0x647462, 0x83927a, 0x9caa8a, 0x4d5d4c, 0x3d4b41, 0xa3ad8a,
        ],
        Guardian::Codex => [
            0x172c2c, 0x385450, 0x47655d, 0x617d6b, 0x2c4341, 0x2d4543, 0x829779,
        ],
        Guardian::Pi => [
            0x222332, 0x353747, 0x414555, 0x51586a, 0x292e3e, 0x2e3344, 0x797a8d,
        ],
        Guardian::OpenCode => [
            0x202733, 0x3b4a58, 0x50606b, 0x607782, 0x293b46, 0x30444c, 0x8ba1a4,
        ],
        Guardian::DeepSeek => [
            0x142f3d, 0x516c69, 0x7c9180, 0xa6b49a, 0x385c59, 0x335457, 0x9cad94,
        ],
        Guardian::Copilot => [
            0x2d3a4f, 0x687b89, 0x8a9ba1, 0xb0bbb4, 0x4b606f, 0x425768, 0xb6c4bc,
        ],
    };
    for (ix, color) in [
        ink::DEPTH,
        ink::FLOOR_SHADE,
        ink::FLOOR,
        ink::FLOOR_LIGHT,
        ink::SEAM,
        ink::WALL,
        ink::WALL_LIGHT,
    ]
    .into_iter()
    .zip(floor)
    {
        colors[ix as usize] = rgb(color).into();
    }
    let core = match guardian {
        Guardian::Claude => [0xa34837, 0xffb951, 0xffe7ac],
        Guardian::Codex => [0x9c7944, 0xffd58a, 0xffefc5],
        Guardian::Pi => [0x8c8a77, 0xeee5c7, 0xfffae8],
        Guardian::OpenCode => [0x747384, 0xdcdbe9, 0xfffff4],
        Guardian::DeepSeek => [0x275dc4, 0x59c7ff, 0xc5f8ff],
        Guardian::Copilot => [0x1460a5, 0x42bbef, 0xc2f5ff],
    };
    for (ix, value) in core.into_iter().enumerate() {
        colors[ink::CORE_DARK as usize + ix] = rgb(value).into();
    }
    if matches!(guardian, Guardian::DeepSeek | Guardian::Copilot) {
        for (ix, value) in [0x17376e, 0x2b88cf, 0x81dce9].into_iter().enumerate() {
            colors[ink::WATER_DARK as usize + ix] = rgb(value).into();
        }
        colors[ink::GOLD_DARK as usize] = rgb(0x607a88).into();
        colors[ink::DUST as usize] = rgb(0xa4b4bc).into();
        colors[ink::IVORY as usize] = rgb(0xe0eae5).into();
        colors[ink::WHITE as usize] = rgb(0xf3fff8).into();
    }
    if guardian == Guardian::OpenCode {
        colors[ink::WHITE as usize] = rgb(0xfffef5).into();
        colors[ink::IVORY as usize] = rgb(0xe9e7e9).into();
    }
    colors
}

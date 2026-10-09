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
        Guardian::Claude => [0x593b38, 0x8a493b, 0xbc6449, 0xe18a63, 0xf2b88a],
        Guardian::Codex => [0x254039, 0x3d6250, 0x648975, 0x9ebca1, 0xd3dfbc],
        Guardian::Pi => [0x454051, 0x766978, 0xb8ac9c, 0xe6d6b3, 0xf7ebcb],
        Guardian::OpenCode => [0x26353b, 0x485963, 0x7d9194, 0xb9cdca, 0xe2e9d8],
        Guardian::DeepSeek => [0x203451, 0x355785, 0x507daf, 0x7daed0, 0xb5d5de],
        Guardian::Copilot => [0x30394c, 0x515e75, 0x8295a8, 0xb7c6cc, 0xe4e0d2],
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
        Guardian::Claude => [0x683630, 0xeaaa69, 0xffddb2],
        Guardian::Codex => [0x224d4d, 0x7de1c0, 0xd8ffe1],
        Guardian::Pi => [0x673f68, 0xd789c1, 0xffd6eb],
        Guardian::OpenCode => [0x25455d, 0x79d2e6, 0xd9ffff],
        Guardian::DeepSeek => [0x663c61, 0xe488ac, 0xffdaed],
        Guardian::Copilot => [0x645337, 0xedc77a, 0xffedb8],
    };
    for (ix, value) in core.into_iter().enumerate() {
        colors[ink::CORE_DARK as usize + ix] = rgb(value).into();
    }
    colors
}

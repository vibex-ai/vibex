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
        Guardian::Claude => [0x52262c, 0x9a4834, 0xd97757, 0xeea083, 0xffd0b5],
        Guardian::Codex => [0x2c2927, 0x564b3d, 0x8f7958, 0xc3aa79, 0xecdaa3],
        Guardian::Pi => [0x0c1119, 0x1a222a, 0x303a42, 0x4d5860, 0x747d80],
        Guardian::OpenCode => [0x37474f, 0x607d8b, 0x90a4ae, 0xcfd8dc, 0xeef3f5],
        Guardian::DeepSeek => [0x1b2370, 0x2f3fb8, 0x4d6bfe, 0x7d94ff, 0xb9c6ff],
        Guardian::Copilot => [0x11393e, 0x155958, 0x228e80, 0x5ac8a4, 0xa3e8bd],
        // Graphite, as the logo's dark cube: its lit faces stay cool and grey.
        Guardian::Cursor => [0x0e1013, 0x1b1e23, 0x2b2f36, 0x464b55, 0x737a86],
        // The logo's white outline, as a light steel frame.
        Guardian::Cline => [0x262b36, 0x4f5767, 0x8f98a8, 0xc9d0db, 0xf2f4f8],
        // The white ring and slash of the logo on dark.
        Guardian::Grok => [0x34363d, 0x6a6e78, 0xa9adb6, 0xdcdee3, 0xffffff],
        // Dark slate stone with the logo's crisp edges catching the light.
        Guardian::ZCode => [0x10141c, 0x1f2633, 0x343d4f, 0x55627a, 0x8d9ab3],
        // The logo's black tile.
        Guardian::Kimi => [0x020203, 0x0c0c0f, 0x19191e, 0x2c2c33, 0x4a4a54],
        // The mascot's white, cooled toward the logo's lilac in shadow.
        Guardian::CodeBuddy => [0x3a3260, 0x6e66a8, 0xb2acdc, 0xe2def5, 0xffffff],
        // The arch wears the gradient inks; its body ramp only lights edges.
        Guardian::Antigravity => [0x2b3242, 0x56607a, 0x8d97b0, 0xc4cbdb, 0xf1f4f9],
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
            0x0f2233, 0x9fbccb, 0xcfe3ea, 0xeef8fb, 0x87a9ba, 0x6f93a8, 0xb7d3df,
        ],
        Guardian::Copilot => [
            0x2d3a4f, 0x687b89, 0x8a9ba1, 0xb0bbb4, 0x4b606f, 0x425768, 0xb6c4bc,
        ],
        // A pale editor grid, so the dark cube reads as the logo does on white.
        Guardian::Cursor => [
            0x1a1e26, 0x9aa2ab, 0xb4bac1, 0xcdd1d6, 0x8a929c, 0x5d6672, 0xdfe2e6,
        ],
        // A dark server room, so the white head stands out as the logo does.
        Guardian::Cline => [
            0x080c18, 0x1a2236, 0x222c45, 0x2d3955, 0x121a2c, 0x34405f, 0x52618a,
        ],
        // A black observatory floor under a field of stars.
        Guardian::Grok => [
            0x020306, 0x0d0f18, 0x121522, 0x1a1e30, 0x08090f, 0x232840, 0x3a4163,
        ],
        // A sandstone gate hall, warm against the cold slab.
        Guardian::ZCode => [
            0x2a1f17, 0x8a7356, 0xa68c69, 0xbfa57f, 0x6e5a42, 0x5c4a36, 0xd4bd96,
        ],
        // A pale ruled page, so the black tile and its blue trail stand out.
        Guardian::Kimi => [
            0x1e1d1b, 0xc4c0b5, 0xdcd8ce, 0xece9e1, 0xb1ada2, 0x86827a, 0xf6f4ee,
        ],
        // A deep violet playroom, the logo's gradient underfoot.
        Guardian::CodeBuddy => [
            0x120e26, 0x2d2559, 0x3a3170, 0x4a3f8a, 0x231d48, 0x2b2453, 0x6c5fc0,
        ],
        // A pale plaza, so the gradient arch reads as the logo does on white.
        Guardian::Antigravity => [
            0x1c2230, 0xc6ceda, 0xdbe1e9, 0xedf1f5, 0xb2bcc9, 0x8a95a5, 0xf8fafc,
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
        Guardian::Claude => [0x9e2b25, 0xff6b4a, 0xffd2b0],
        Guardian::Codex => [0x9c7944, 0xffd58a, 0xffefc5],
        Guardian::Pi => [0x9a7b3c, 0xffd36e, 0xfff6d8],
        Guardian::OpenCode => [0xa0522d, 0xffb347, 0xfff1c9],
        Guardian::DeepSeek => [0xb07a1a, 0xffd04a, 0xfff4c2],
        Guardian::Copilot => [0x9b2c2c, 0xff7a45, 0xffe0b0],
        // The pointer facet charges cyan-white.
        Guardian::Cursor => [0x236a94, 0x5cd2ff, 0xe4fbff],
        // The vent on the back of the head runs hot.
        Guardian::Cline => [0xa1461c, 0xff8a3d, 0xffe2c4],
        // The core in the back of the ring burns red.
        Guardian::Grok => [0x8a1022, 0xff3a52, 0xffc6cd],
        // The core behind the shutter, and the sweep, burn electric violet.
        Guardian::ZCode => [0x4b1fa0, 0x9d6bff, 0xe6dcff],
        // The logo's blue square, and the context it lays down.
        Guardian::Kimi => [0x0b4fb3, 0x1683ff, 0xb3d6ff],
        // The teal glow at the foot of the logo.
        Guardian::CodeBuddy => [0x0f7a62, 0x32e6b9, 0xc5fff0],
        // The logo's red, for the right of the arch and its heart.
        Guardian::Antigravity => [0xa3171b, 0xfc413d, 0xffc2bd],
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
    if guardian == Guardian::Cline {
        // Its eyes and bolts are a bright terminal cyan.
        for (ix, value) in [0x0f4a7a, 0x2fa8f0, 0xa8ecff].into_iter().enumerate() {
            colors[ink::WATER_DARK as usize + ix] = rgb(value).into();
        }
    }
    if guardian == Guardian::Antigravity {
        // The rest of the logo's gradient: green, yellow and blue.
        for (ix, value) in [0x006b35, 0x00b95c, 0x7de0a6].into_iter().enumerate() {
            colors[ink::MOSS_DARK as usize + ix] = rgb(value).into();
        }
        colors[ink::GOLD_DARK as usize] = rgb(0xc9a300).into();
        colors[ink::GOLD as usize] = rgb(0xffe432).into();
        colors[ink::IVORY as usize] = rgb(0xfff6c2).into();
        for (ix, value) in [0x1b4fbf, 0x3186ff, 0x9fc6ff].into_iter().enumerate() {
            colors[ink::WATER_DARK as usize + ix] = rgb(value).into();
        }
    }
    if guardian == Guardian::CodeBuddy {
        // The visor is the logo's violet-to-blue gradient, the eyes its
        // cool white.
        for (ix, value) in [0x3b2a9e, 0x5b4fe0, 0x8f9bff].into_iter().enumerate() {
            colors[ink::WATER_DARK as usize + ix] = rgb(value).into();
        }
        colors[ink::WHITE as usize] = rgb(0xffffff).into();
        colors[ink::IVORY as usize] = rgb(0xeae8fb).into();
        colors[ink::DUST as usize] = rgb(0xb9b4e2).into();
    }
    if guardian == Guardian::Kimi {
        // The K is the logo's pure white.
        colors[ink::WHITE as usize] = rgb(0xffffff).into();
        colors[ink::IVORY as usize] = rgb(0xe4e4e8).into();
        colors[ink::DUST as usize] = rgb(0xb4b4bc).into();
    }
    if guardian == Guardian::Cursor {
        // The logo is monochrome: its light facet is a neutral white.
        colors[ink::WHITE as usize] = rgb(0xf5f7fa).into();
        colors[ink::IVORY as usize] = rgb(0xd9dde3).into();
        colors[ink::DUST as usize] = rgb(0xa6adb7).into();
    }
    if guardian == Guardian::OpenCode {
        colors[ink::WHITE as usize] = rgb(0xfffef5).into();
        colors[ink::IVORY as usize] = rgb(0xe9e7e9).into();
    }
    colors
}

//! Design tokens for the native mobile client.
//!
//! Values define the shared mobile palette and spacing scale so the two GPUI
//! phone clients read as one product. Views must go through these accessors
//! instead of hardcoding colors.

use gpui::{Hsla, hsla, rgb};

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

// The values below mirror the shared desktop dark tokens
// (`crates/vibex-ui/theme/tokens.json`) so the phone reads as the same product
// as the desktop shell: the session page uses `background`, the sessions page
// uses `sidebar`, and the workbench page uses the right-rail surfaces.
pub const BG_PRIMARY: u32 = 0x09090b;
pub const BG_CARD: u32 = 0x18181b;
/// Lower-contrast card fill than [`BG_CARD`]; blends into [`BG_PRIMARY`] on dense lists.
pub const BG_CARD_DIM: u32 = 0x1c1c1e;
pub const TEXT_PRIMARY: u32 = 0xfafafa;
pub const TEXT_SECONDARY: u32 = 0xd4d4d8;
pub const TEXT_MUTED: u32 = 0x9f9fa9;
/// Desktop borders are pure white at 10%/6%; these are the flattened values
/// over [`BG_PRIMARY`] for the places that still need an opaque fill.
pub const BORDER_DEFAULT: u32 = 0x232326;
pub const BORDER_SUBTLE: u32 = 0x18181b;
pub const ACCENT_GREEN: u32 = 0x00c950;
pub const ACCENT_BLUE: u32 = 0x51a2ff;
pub const ACCENT_YELLOW: u32 = 0xefb000;
pub const ACCENT_RED: u32 = 0xff6467;
pub const ACCENT_DIM: u32 = 0x71717b;
pub const ACCENT_PURPLE: u32 = 0xc678dd;

pub const SIDEBAR_BG: u32 = 0x18181b;
pub const SIDEBAR_SELECTED_BG: u32 = 0x27272a;
pub const SIDEBAR_TEXT_PRIMARY: u32 = 0xfafafa;
pub const SIDEBAR_TEXT_SECONDARY: u32 = 0xd4d4d8;
pub const SIDEBAR_TEXT_MUTED: u32 = 0x9f9fa9;

/// Workbench page surfaces mirror the desktop right rail: the activity bar and
/// panel body sit on `background`, and the panel chrome uses `sidebar`.
pub const WORKBENCH_BG: u32 = 0x09090b;
pub const WORKBENCH_PANEL_BG: u32 = 0x18181b;

// ---------------------------------------------------------------------------
// Spacing and typography
// ---------------------------------------------------------------------------

pub const SPACING_XS: f32 = 4.0;
pub const SPACING_SM: f32 = 8.0;
pub const SPACING_MD: f32 = 12.0;
pub const SPACING_LG: f32 = 16.0;
pub const SPACING_XL: f32 = 20.0;

pub const FONT_APP_TITLE: f32 = 28.0;
pub const FONT_HEADING: f32 = 13.0;
pub const FONT_BODY: f32 = 12.0;
pub const FONT_DETAIL: f32 = 12.0;
pub const FONT_CAPTION: f32 = 11.0;
/// Metadata that must not compete with the caption it annotates (timestamps,
/// workspace paths, row counts).
pub const FONT_MICRO: f32 = 10.0;

pub const ICON_MD: f32 = 18.0;
pub const ICON_SM: f32 = 16.0;
pub const ICON_STATUS: f32 = 6.0;

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// The compact top bar stays below the native status-bar inset supplied by GPUI.
pub const HEADER_HEIGHT: f32 = 40.0;
pub const HEADER_BUTTON_SIZE: f32 = 40.0;
/// Drawer and overlay headers have a little more room for the compact action
/// cluster while the session page keeps the shorter workbench header.
pub const DRAWER_HEADER_HEIGHT: f32 = 52.0;
/// Minimum tappable edge for controls that are not header buttons.
pub const TOUCH_TARGET: f32 = 44.0;
/// Caps centered panel content so tablets do not stretch a phone layout.
pub const CARD_WIDTH: f32 = 300.0;

pub const RADIUS_CARD: f32 = 8.0;
pub const RADIUS_CONTROL: f32 = 6.0;

pub const DRAWER_ROW_HEIGHT: f32 = 52.0;
pub const SIDEBAR_ROW_HEIGHT: f32 = 44.0;
/// Indent per tree level, matching the desktop's nested sidebar spacing.
pub const SIDEBAR_INDENT: f32 = 14.0;
/// Width of one trailing sidebar affordance column. Project/session/folder
/// rows reserve two columns (menu plus grip); Worktree rows reserve one menu
/// column and use a long press for moving.
pub const SIDEBAR_GRIP_WIDTH: f32 = 34.0;
pub const DRAWER_ACTION_HEIGHT: f32 = 40.0;
pub const DRAWER_SECTION_HEIGHT: f32 = 40.0;
pub const DRAWER_DRAG_THRESHOLD: f32 = 6.0;
pub const DRAWER_VERTICAL_CANCEL_RATIO: f32 = 2.0;
/// Fraction of the viewport a page must travel before settling on the next page.
pub const DRAWER_SNAP_TRAVEL_RATIO: f32 = 0.12;
/// Release-adjacent movement in the intended direction commits the transition.
pub const DRAWER_SNAP_COMMIT_DIRECTION_THRESHOLD: f32 = 1.5;
/// Reversing a transition requires a much clearer release-adjacent movement.
pub const DRAWER_SNAP_REVERSE_DIRECTION_THRESHOLD: f32 = 28.0;
pub const DRAWER_BACKDROP_OPACITY: f32 = 0.4;
pub const DRAWER_OPEN_ANIMATION_MS: u64 = 160;
pub const DRAWER_CLOSE_ANIMATION_MS: u64 = 100;

// ---------------------------------------------------------------------------
// Accessors
// ---------------------------------------------------------------------------

pub fn bg_primary() -> Hsla {
    rgb(BG_PRIMARY).into()
}

pub fn bg_card() -> Hsla {
    rgb(BG_CARD).into()
}

pub fn bg_card_dim() -> Hsla {
    rgb(BG_CARD_DIM).into()
}

pub fn text_primary() -> Hsla {
    rgb(TEXT_PRIMARY).into()
}

pub fn text_secondary() -> Hsla {
    rgb(TEXT_SECONDARY).into()
}

pub fn text_muted() -> Hsla {
    rgb(TEXT_MUTED).into()
}

pub fn border_default() -> Hsla {
    rgb(BORDER_DEFAULT).into()
}

pub fn border_subtle() -> Hsla {
    rgb(BORDER_SUBTLE).into()
}

pub fn row_pressed_bg() -> Hsla {
    hsla(0.0, 0.0, 1.0, 0.10)
}

pub fn sidebar_drop_bg() -> Hsla {
    hsla(0.58, 0.9, 0.6, 0.18)
}

pub fn workbench_bg() -> Hsla {
    rgb(WORKBENCH_BG).into()
}

pub fn workbench_panel_bg() -> Hsla {
    rgb(WORKBENCH_PANEL_BG).into()
}

pub fn sidebar_bg() -> Hsla {
    rgb(SIDEBAR_BG).into()
}

pub fn sidebar_selected_bg() -> Hsla {
    rgb(SIDEBAR_SELECTED_BG).into()
}

pub fn sidebar_text_primary() -> Hsla {
    rgb(SIDEBAR_TEXT_PRIMARY).into()
}

pub fn sidebar_text_secondary() -> Hsla {
    rgb(SIDEBAR_TEXT_SECONDARY).into()
}

pub fn sidebar_text_muted() -> Hsla {
    rgb(SIDEBAR_TEXT_MUTED).into()
}

pub fn backdrop(opacity: f32) -> Hsla {
    hsla(0.0, 0.0, 0.0, opacity)
}

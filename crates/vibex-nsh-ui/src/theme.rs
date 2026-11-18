//! The phone's design tokens and type ladder.
//!
//! Colors are `nsh`'s theme tokens, generated into `apps/mobile-nsh/theme.json`
//! from the shared source `crates/vibex-ui/theme/tokens.json` (the same file the
//! desktop and the GPUI mobile client draw with), so both clients read one
//! palette. Names resolve through [`theme`] and are resolved for the current
//! appearance by `nsh`; a screen reads [`foreground`], never a hex value.
//!
//! The spacing, radius and type constants are the phone client's own ladder
//! (`apps/mobile/src/theme.rs`). They stay here while the GPUI client is still
//! shipping, so a screen migrated to `nsh` keeps the numbers its pair has.

use nsh_core::prelude::*;

pub use nsh_base::theme;

/// The smallest step: icon-to-label gaps inside a control.
pub const SPACING_XS: f32 = 4.0;
/// The gap between the entries of an action stack.
pub const SPACING_SM: f32 = 8.0;
/// Padding inside a sheet and between a row's icon and its text.
pub const SPACING_MD: f32 = 12.0;
/// Padding around a page's content.
pub const SPACING_LG: f32 = 16.0;
/// The page's outer gutter, which the pairing page uses.
pub const SPACING_XL: f32 = 20.0;

/// The corner radius of a control (buttons, fields, rows).
///
/// The phone's own step is 8 points; the shared token source's control radius
/// is 6, which the desktop uses. The migrated screen keeps the same values as
/// its GPUI pair, so the two clients stay hard to tell apart.
pub const RADIUS_CONTROL: f32 = 8.0;

/// The minimum touch target the phone keeps for every pressable row.
pub const TOUCH_TARGET: f32 = 44.0;

/// A small icon (inside a control).
pub const ICON_SM: f32 = 16.0;
/// A list row's icon.
pub const ICON_MD: f32 = 18.0;

/// The page title.
pub const FONT_TITLE: f32 = 22.0;
/// A section heading, and a prominent sheet action.
pub const FONT_HEADING: f32 = 18.0;
/// Body copy: message text, field values, button labels.
pub const FONT_BODY: f32 = 17.0;
/// Detail copy: a row's label, a compact button's label.
pub const FONT_DETAIL: f32 = 15.0;
/// Captions and metadata.
pub const FONT_CAPTION: f32 = 14.0;
/// The smallest supporting copy.
pub const FONT_MICRO: f32 = 13.0;
/// The large numerals of a verification code.
pub const FONT_DISPLAY: f32 = 30.0;
/// The product wordmark.
pub const FONT_WORDMARK: f32 = 36.0;

/// The line-height ratio the phone uses for wrapped body copy.
pub const LINE_HEIGHT_RATIO: f32 = 1.45;

/// The adaptive color named `name` (one value per appearance).
pub fn adaptive_token(name: &str) -> AdaptiveColor {
    theme().color(name).unwrap_or(AdaptiveColor::TRANSPARENT)
}

/// The concrete color named `name`, resolved for the current appearance.
///
/// Custom tokens (`sidebar-border`, the file-icon colors) come from the same
/// generated theme file, so a name the token source adds reaches a screen
/// without a code change.
pub fn color_token(name: &str) -> Hsla {
    color(adaptive_token(name))
}

/// The app's page background.
pub fn background() -> Hsla {
    color(theme().background)
}

/// Text and icons on the page background.
pub fn foreground() -> Hsla {
    color(theme().foreground)
}

/// The main action's fill.
pub fn primary() -> Hsla {
    color(theme().primary)
}

/// Text on [`primary`].
pub fn primary_foreground() -> Hsla {
    color(theme().primary_foreground)
}

/// Secondary text: hints, captions, placeholders.
pub fn muted_foreground() -> Hsla {
    color(theme().muted_foreground)
}

/// Body copy one step below the foreground: a row's value, the description
/// under a title.
///
/// The GPUI client mixes 47% of the muted foreground into the foreground; the
/// same blend is kept here so the two stacks read the same contrast.
pub fn text_secondary() -> Hsla {
    foreground().mix(muted_foreground(), 0.53)
}

/// Supporting copy: metadata, captions, descriptions.
pub fn text_muted() -> Hsla {
    muted_foreground().mix(foreground(), 0.65)
}

/// The surface a card or a sheet rests on.
pub fn background_card() -> Hsla {
    color(theme().card)
}

/// The dimmed fill dense rows sit on.
pub fn background_dim() -> Hsla {
    color_token("secondary")
}

/// The default divider color.
pub fn border_default() -> Hsla {
    color(theme().border)
}

/// The quieter divider a list uses between its rows.
pub fn border_subtle() -> Hsla {
    color_token("sidebar-border")
}

/// The stroke of an input at rest.
pub fn border_input() -> Hsla {
    color(theme().input)
}

/// The primary action's fill (the phone paints it with the foreground tone).
pub fn action_primary() -> Hsla {
    foreground()
}

/// The label on a primary action.
pub fn action_primary_label() -> Hsla {
    background()
}

/// A destructive action and an error.
pub fn accent_red() -> Hsla {
    color(theme().destructive)
}

/// The body line height for a text size in points.
pub fn line_height_for(size: f32) -> Points {
    px(size * LINE_HEIGHT_RATIO)
}

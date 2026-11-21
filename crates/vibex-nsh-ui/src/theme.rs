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

use std::cell::Cell;

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

/// The corner radius of a card: a saved runtime's row, a dialog.
pub const RADIUS_CARD: f32 = 10.0;

/// The corner radius of a bottom sheet's top edge.
pub const RADIUS_SHEET: f32 = 22.0;

/// How far a bottom sheet dims the page behind it.
pub const SHEET_BACKDROP_OPACITY: f32 = 0.55;

/// The pull handle at the top of a bottom sheet.
pub const SHEET_GRABBER_WIDTH: f32 = 44.0;
/// The pull handle's thickness.
pub const SHEET_GRABBER_HEIGHT: f32 = 5.0;

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

/// The text-size steps the Settings page offers, as percentages of the ladder.
pub const FONT_SCALE_STEPS: [u32; 4] = [80, 90, 100, 115];

thread_local! {
    /// The text-size step every font on the ladder is drawn at. One device
    /// draws its one app on one engine thread; tests run several apps on
    /// their own threads.
    static FONT_SCALE_PERCENT: Cell<u32> = const { Cell::new(100) };
}

/// Applies a text-size step to every font the screens read.
pub fn set_font_scale_percent(percent: u32) {
    FONT_SCALE_PERCENT.with(|cell| cell.set(percent.clamp(75, 150)));
}

/// The text-size step in use.
pub fn font_scale_percent() -> u32 {
    FONT_SCALE_PERCENT.with(Cell::get)
}

fn scaled_font(base: f32) -> f32 {
    base * font_scale_percent() as f32 / 100.0
}

/// The page title at the current text-size step.
pub fn font_title() -> f32 {
    scaled_font(FONT_TITLE)
}

/// A section heading at the current text-size step.
pub fn font_heading() -> f32 {
    scaled_font(FONT_HEADING)
}

/// Body copy at the current text-size step.
pub fn font_body() -> f32 {
    scaled_font(FONT_BODY)
}

/// Detail copy at the current text-size step.
pub fn font_detail() -> f32 {
    scaled_font(FONT_DETAIL)
}

/// Captions at the current text-size step.
pub fn font_caption() -> f32 {
    scaled_font(FONT_CAPTION)
}

/// Supporting copy at the current text-size step.
pub fn font_micro() -> f32 {
    scaled_font(FONT_MICRO)
}

/// Large numerals at the current text-size step.
pub fn font_display() -> f32 {
    scaled_font(FONT_DISPLAY)
}

/// The wordmark at the current text-size step.
pub fn font_wordmark() -> f32 {
    scaled_font(FONT_WORDMARK)
}

/// The step after `current`, wrapping at the top; an off-ladder value snaps to
/// the nearest step so the row always names a rung.
pub fn next_font_scale_percent(current: u32) -> u32 {
    match FONT_SCALE_STEPS.iter().position(|step| *step == current) {
        Some(index) => FONT_SCALE_STEPS[(index + 1) % FONT_SCALE_STEPS.len()],
        None => *FONT_SCALE_STEPS
            .iter()
            .min_by_key(|step| step.abs_diff(current))
            .unwrap_or(&100),
    }
}

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

/// The page behind a column of filled blocks.
///
/// The blocks are the lighter surface in both appearances: the card tone in
/// the dark palette, white in the light one, where the page takes the
/// secondary grey behind them.
pub fn block_page() -> Hsla {
    if background_card() == background() {
        color_token("secondary")
    } else {
        background()
    }
}

/// The filled block a settings group rests on.
pub fn block_surface() -> Hsla {
    background_card()
}

/// A well inside a block: a value preview, an icon tile, a switch track.
pub fn block_inset() -> Hsla {
    color_token("muted")
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

/// The pairing wordmark's mark geometry: the GPUI client's authored 42 pt
/// lockup, scaled to the label's size. The mark is wider than the label is
/// tall and its point lands on the baseline.
pub struct WordmarkMark {
    pub width: f32,
    pub height: f32,
    pub margin_top: f32,
    pub top_offset: f32,
}

/// The mark's box at `font` points, from the 42 pt authored lockup.
pub fn wordmark_mark(font: f32) -> WordmarkMark {
    let scale = font / 42.0;
    WordmarkMark {
        width: 46.0 * scale,
        height: 45.0 * scale,
        margin_top: 2.0 * scale,
        top_offset: -6.0 * scale,
    }
}

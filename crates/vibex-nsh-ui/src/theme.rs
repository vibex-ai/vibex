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
/// Footer notes and the quietest captions.
pub const FONT_HINT: f32 = 12.0;
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

/// Footer notes at the current text-size step.
pub fn font_hint() -> f32 {
    scaled_font(FONT_HINT)
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

/// The card surface as an adaptive color, for a style prop that follows the
/// appearance on its own.
pub fn card_adaptive() -> AdaptiveColor {
    theme().card
}

/// The default border as an adaptive color.
pub fn border_adaptive() -> AdaptiveColor {
    theme().border
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

// ---------------------------------------------------------------------------
// The session workspace: the conversation page and the two pages beside it
// ---------------------------------------------------------------------------

/// The conversation page's top bar, below the status bar.
pub const HEADER_HEIGHT: f32 = 48.0;
/// A leading header control: a full touch target.
pub const HEADER_BUTTON_SIZE: f32 = 44.0;
/// The search and rail controls at the header's trailing edge. They read as a
/// pair, so each is narrower than the leading menu while keeping the header's
/// whole height for the thumb.
pub const HEADER_TRAILING_BUTTON_SIZE: f32 = 36.0;
/// The header of a side page and of a sheet.
pub const DRAWER_HEADER_HEIGHT: f32 = 56.0;
/// The strip that names the tree and carries its tools.
pub const DRAWER_SECTION_HEIGHT: f32 = 40.0;
/// How far a side page dims the conversation behind it when fully open.
pub const DRAWER_BACKDROP_OPACITY: f32 = 0.4;
/// How long a side page takes to open, and to close.
pub const DRAWER_OPEN_ANIMATION_MS: f32 = 160.0;
pub const DRAWER_CLOSE_ANIMATION_MS: f32 = 100.0;
/// The share of the page a drag must reveal before a slow release settles on
/// the next page.
pub const DRAWER_SNAP_TRAVEL_RATIO: f32 = 0.15;
/// A release at least this fast (points per second) is a swipe.
pub const DRAWER_SWIPE_VELOCITY: f32 = 600.0;
/// How much faster across than along a release must be to count as one.
pub const DRAWER_SWIPE_STRAIGHTNESS: f32 = 1.5;

// The sessions page is a drawer beside the conversation rather than a page
// over it: the conversation is pushed aside as a card and a strip of it stays
// in reach, so the way back is always under the thumb.

/// The widest the sessions drawer grows.
pub const SESSIONS_DRAWER_MAX_WIDTH: f32 = 360.0;
/// The share of the screen it takes on a phone.
pub const SESSIONS_DRAWER_WIDTH_RATIO: f32 = 0.86;
/// The strip of the conversation that always stays beside it.
pub const SESSIONS_DRAWER_PEEK: f32 = 52.0;
/// The corner the conversation takes while it is a card beside the drawer.
pub const SESSIONS_CARD_RADIUS: f32 = 28.0;
/// How much shorter the card stands with the drawer fully open.
pub const SESSIONS_CARD_SHRINK: f32 = 0.05;
/// How far the card fades into the page behind it.
pub const SESSIONS_CARD_FADE: f32 = 0.5;
/// The card travels with the drawer, so it takes longer than a page that
/// slides over a still one.
pub const SESSIONS_OPEN_ANIMATION_MS: f32 = 260.0;
pub const SESSIONS_CLOSE_ANIMATION_MS: f32 = 200.0;

/// The width of the sessions drawer on a screen `screen` points wide.
pub fn sessions_drawer_width(screen: f32) -> f32 {
    SESSIONS_DRAWER_MAX_WIDTH
        .min(screen * SESSIONS_DRAWER_WIDTH_RATIO)
        .min(screen - SESSIONS_DRAWER_PEEK)
        .max(1.0)
}

/// A status dot beside a runtime's name.
pub const ICON_STATUS: f32 = 6.0;

/// The prompt card's radius: rounder than a list card, because it is the one
/// surface the reader writes in.
pub const RADIUS_COMPOSER: f32 = 14.0;
/// A pill inside a control row.
pub const CHIP_HEIGHT: f32 = 32.0;
/// The composer's primary control.
pub const COMPOSER_SEND_SIZE: f32 = 36.0;
/// One icon control on the composer's row.
pub const COMPOSER_OPTION_SIZE: f32 = 36.0;
/// The composer's own text step, a rung above body copy.
pub const FONT_COMPOSER: f32 = 19.0;

/// The composer field at the current text-size step.
pub fn font_composer() -> f32 {
    scaled_font(FONT_COMPOSER)
}

// The tree's geometry. Every row is the same height and has the same columns:
// a disclosure slot, a mark slot, the title, and what trails it. A level of
// nesting moves a row one disclosure slot to the right, so a child's
// disclosure sits under its parent's mark.

/// Horizontal padding of the row list.
pub const SIDEBAR_LIST_PADDING: f32 = 12.0;
/// The padding inside a row's plate, on both sides.
pub const SIDEBAR_ROW_INSET: f32 = 8.0;
/// The gap a row keeps above and below its plate.
pub const SIDEBAR_ROW_GAP: f32 = 2.0;
/// The disclosure slot at the start of a row.
pub const SIDEBAR_CHEVRON_SLOT: f32 = 16.0;
/// The disclosure glyph.
pub const SIDEBAR_CHEVRON_SIZE: f32 = 12.0;
/// The mark slot: an Agent, a project, a folder, or the selection mark.
pub const SIDEBAR_ICON_SLOT: f32 = 24.0;
/// The gap between the mark slot and the title.
pub const SIDEBAR_ICON_TITLE_GAP: f32 = 6.0;
/// Indent of a project's own sessions.
pub const SIDEBAR_PROJECT_SESSION_INDENT: f32 = SIDEBAR_CHEVRON_SLOT;
/// Indent of the sessions inside a worktree card.
pub const SIDEBAR_WORKSPACE_SESSION_INDENT: f32 = SIDEBAR_CHEVRON_SLOT;
/// Indent of the rows filed inside a folder.
pub const SIDEBAR_FOLDER_CHILD_INDENT: f32 = SIDEBAR_CHEVRON_SLOT;
/// Where a folder draws the guide down its child column: under the middle of
/// its own disclosure.
pub const SIDEBAR_FOLDER_GUIDE_OFFSET: f32 = SIDEBAR_ROW_INSET + SIDEBAR_CHEVRON_SLOT / 2.0;
/// The radius of a row's plate.
pub const SIDEBAR_ROW_RADIUS: f32 = 12.0;
/// The radius of the card around a worktree and its sessions.
pub const SIDEBAR_CARD_RADIUS: f32 = 14.0;
/// How far a row's plate stands in from the edges of the card around it.
pub const SIDEBAR_CARD_INSET: f32 = 4.0;
/// The Agent mark on a session row.
pub const SIDEBAR_AGENT_LOGO_SIZE: f32 = 16.0;
/// Project and folder marks.
pub const SIDEBAR_PROJECT_LOGO_SIZE: f32 = 16.0;
/// A worktree or session status dot.
pub const SIDEBAR_STATUS_DOT: f32 = 8.0;
/// The unread dot, deliberately smaller than a status dot.
pub const SIDEBAR_UNREAD_DOT: f32 = 7.0;
/// The glyphs trailing a row: a pin, a pause, a schedule.
pub const SIDEBAR_META_ICON: f32 = 13.0;
/// The round control at the end of a project or worktree row.
pub const SIDEBAR_ROW_ACTION: f32 = 28.0;
/// The mark a row wears while sessions are being selected.
pub const SIDEBAR_SELECTION_MARK: f32 = 20.0;
/// The floor of a tree row's height.
pub const SIDEBAR_ROW_HEIGHT: f32 = 48.0;

/// The tree's own type steps.
pub const FONT_SIDEBAR_ROW: f32 = FONT_BODY;
pub const FONT_SIDEBAR_META: f32 = FONT_MICRO;

/// A row's title at the current text-size step.
pub fn font_sidebar_row() -> f32 {
    scaled_font(FONT_SIDEBAR_ROW)
}

/// A row's metadata at the current text-size step.
pub fn font_sidebar_meta() -> f32 {
    scaled_font(FONT_SIDEBAR_META)
}

/// The height of a tree row: the two lines a worktree row stacks inside its
/// plate, never below the touch-target floor, so a larger text step does not
/// clip the second.
pub fn sidebar_row_height() -> f32 {
    let lines = (font_sidebar_row() + font_hint()) * LINE_HEIGHT_RATIO;
    (lines + SIDEBAR_ROW_GAP * 2.0).max(SIDEBAR_ROW_HEIGHT)
}

/// The surface of the two side pages.
pub fn sidebar_bg() -> Hsla {
    color_token("sidebar")
}

/// The plate of the selected row on a side page.
pub fn sidebar_selected_bg() -> Hsla {
    color_token("sidebar-accent")
}

/// Tool glyphs and the footer's label.
pub fn sidebar_text_secondary() -> Hsla {
    blend(color_token("sidebar-foreground"), muted_foreground(), 0.45)
}

/// The quietest ink on a side page.
pub fn sidebar_text_muted() -> Hsla {
    blend(muted_foreground(), color_token("sidebar-foreground"), 0.12)
}

/// The guide down a folder's child column.
pub fn sidebar_tree_guide() -> Hsla {
    color_token("sidebar-border").opacity(0.70)
}

/// A well on the sessions page: the search field, a round control. A wash of
/// the ink rather than a token of its own, so it reads on the page and on a
/// card alike.
pub fn sidebar_well() -> Hsla {
    foreground().alpha(if dark() { 0.09 } else { 0.055 })
}

/// A hairline on the sessions page.
pub fn sidebar_hairline() -> Hsla {
    border_subtle().opacity(0.6)
}

/// The wash a pressed row or control takes.
pub fn row_pressed_bg() -> Hsla {
    let luminance = if dark() { 0.92 } else { 0.10 };
    hsla(0.0, 0.0, luminance, 0.11)
}

/// The plate the selected surface tab lights up with: dimmer than a pressed
/// row, because a tab is chrome over the surface.
pub fn rail_tab_bg() -> Hsla {
    if dark() {
        hsla(0.0, 0.0, 0.92, 0.09)
    } else {
        hsla(0.0, 0.0, 0.10, 0.07)
    }
}

/// The surface the reader writes on, and a sent message's plate.
pub fn composer_surface() -> Hsla {
    color_token("composer-surface")
}

/// A finished or connected state.
pub fn accent_success() -> Hsla {
    color_token("chart-2")
}

/// The same tone, where a call site names the hue.
pub fn accent_green() -> Hsla {
    accent_success()
}

/// Links, the insertion line and a branch that is behind.
pub fn accent_blue() -> Hsla {
    color_token("chart-category-1")
}

/// A state waiting on the reader.
pub fn accent_yellow() -> Hsla {
    color_token("warning")
}

/// A session group's own mark.
pub fn accent_purple() -> Hsla {
    color_token("chart-category-8")
}

/// The scrim behind a side page or a sheet.
pub fn backdrop(opacity: f32) -> Hsla {
    hsla(0.0, 0.0, 0.0, opacity)
}

/// `from` moved `amount` of the way to `to`, channel by channel, so a step
/// down the ladder moves lightness without letting the page show through.
fn blend(from: Hsla, to: Hsla, amount: f32) -> Hsla {
    let amount = amount.clamp(0.0, 1.0);
    Hsla {
        h: from.h + (to.h - from.h) * amount,
        s: from.s + (to.s - from.s) * amount,
        l: from.l + (to.l - from.l) * amount,
        a: from.a + (to.a - from.a) * amount,
    }
}

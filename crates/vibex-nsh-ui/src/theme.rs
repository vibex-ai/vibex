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
/// The line a blank page greets with.
pub const FONT_GREETING: f32 = 26.0;

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

/// A blank page's greeting at the current text-size step.
pub fn font_greeting() -> f32 {
    scaled_font(FONT_GREETING)
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

/// The page background as an adaptive color, for a gradient that fades into
/// it.
pub fn background_adaptive() -> AdaptiveColor {
    theme().background
}

/// The same color with no opacity: where such a gradient ends, so it fades
/// without passing through another hue.
pub fn background_clear_adaptive() -> AdaptiveColor {
    let page = theme().background;
    AdaptiveColor {
        light: page.light.alpha(0.0),
        dark: page.dark.alpha(0.0),
    }
}

/// [`block_page`] as an adaptive color, for the lip of a sheet.
pub fn page_adaptive() -> AdaptiveColor {
    let card = theme().card;
    let page = theme().background;
    let grey = adaptive_token("secondary");
    AdaptiveColor {
        light: if card.light == page.light {
            grey.light
        } else {
            page.light
        },
        dark: if card.dark == page.dark {
            grey.dark
        } else {
            page.dark
        },
    }
}

/// [`page_adaptive`] with no opacity, for a gradient that fades back into the
/// page it started from.
pub fn page_clear_adaptive() -> AdaptiveColor {
    let page = page_adaptive();
    AdaptiveColor {
        light: page.light.alpha(0.0),
        dark: page.dark.alpha(0.0),
    }
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

/// [`block_page`] as an adaptive color.
pub fn block_page_adaptive() -> AdaptiveColor {
    let page = block_page();
    AdaptiveColor {
        light: page,
        dark: page,
    }
}

/// [`block_page`] as the veil the top band keeps over the conversation.
///
/// Nearly the page's own tone, but not quite: a message still shows through it,
/// which is what tells the reader the transcript goes on under the bar rather
/// than ending there. The band's blur carries the rest of the separation, so
/// this stays lighter than the page would be on its own.
pub fn block_page_veil_adaptive() -> AdaptiveColor {
    let page = block_page_adaptive();
    const VEIL: f32 = 0.82;
    AdaptiveColor {
        light: page.light.alpha(VEIL),
        dark: page.dark.alpha(VEIL),
    }
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
pub const HEADER_HEIGHT: f32 = 52.0;
/// How far a side page dims the conversation behind it when fully open.
pub const DRAWER_BACKDROP_OPACITY: f32 = 0.4;
/// How long a side page takes to open, and to close. A page crosses the whole
/// screen, so it is given the time to be followed by the eye.
pub const DRAWER_OPEN_ANIMATION_MS: f32 = 240.0;
pub const DRAWER_CLOSE_ANIMATION_MS: f32 = 180.0;
/// How far the conversation travels with a side page, as a share of the page's
/// own travel: the two move as one strip at two depths.
pub const PAGE_PARALLAX: f32 = 0.24;
/// The share of the page a drag must reveal before a slow release settles on
/// the next page.
pub const DRAWER_SNAP_TRAVEL_RATIO: f32 = 0.15;
/// A release at least this fast (points per second) is a swipe.
pub const DRAWER_SWIPE_VELOCITY: f32 = 600.0;
/// How much faster across than along a release must be to count as one.
pub const DRAWER_SWIPE_STRAIGHTNESS: f32 = 1.5;

/// A status dot beside a runtime's name.
pub const ICON_STATUS: f32 = 6.0;

/// The prompt card's radius: rounder than a list card, because it is the one
/// surface the reader writes in.
pub const RADIUS_COMPOSER: f32 = 24.0;
/// A pill inside a control row.
pub const CHIP_HEIGHT: f32 = 32.0;
/// The composer's primary control.
pub const COMPOSER_SEND_SIZE: f32 = 36.0;
/// One icon control on the composer's row.
pub const COMPOSER_OPTION_SIZE: f32 = 36.0;
/// The composer's own text step.
pub const FONT_COMPOSER: f32 = FONT_BODY;

// The phone's surfaces. A page is a column of a few kinds of object: a round
// control, a pill, an icon tile at the head of a row, a card of rows, and a
// message bubble. Each kind has one size and one corner wherever it appears.

/// The corner of an icon tile.
pub const RADIUS_TILE: f32 = 9.0;
/// The corner of a chip, a key and a segmented track.
pub const RADIUS_CHIP: f32 = 12.0;
/// The corner of a card of rows, and of a floating menu.
pub const RADIUS_GROUP: f32 = 16.0;
/// The corner of a message bubble, and of the one corner that points at the
/// side its author sits on.
pub const RADIUS_BUBBLE: f32 = 20.0;
pub const RADIUS_BUBBLE_TAIL: f32 = 6.0;
/// The corner of a labelled action.
pub const RADIUS_BUTTON: f32 = 14.0;
/// An icon tile at the head of a row.
pub const TILE_SIZE: f32 = 32.0;
/// The glyph inside a tile.
pub const TILE_ICON: f32 = 17.0;
/// A row of a card: a tile, a title and what trails it.
pub const ROW_HEIGHT: f32 = 52.0;
/// A round control floating on a page.
pub const ROUND_CONTROL: f32 = 40.0;
/// A control inside a pill or a toolbar.
pub const COMPACT_CONTROL: f32 = 34.0;
/// A segmented control's track.
pub const SEGMENT_HEIGHT: f32 = 36.0;
/// A row of a floating menu.
pub const MENU_ROW_HEIGHT: f32 = 46.0;
/// A floating menu's width.
pub const MENU_WIDTH: f32 = 252.0;

/// A wash of the ink: the fill of a well, a pill, an icon tile. A wash rather
/// than a token of its own, so it reads on a page and on a card alike.
pub fn fill() -> Hsla {
    foreground().alpha(if dark() { 0.09 } else { 0.055 })
}

/// The same wash one step stronger, for the chosen one of several fills.
pub fn fill_strong() -> Hsla {
    foreground().alpha(if dark() { 0.16 } else { 0.10 })
}

/// The plate of a message the reader sent.
pub fn bubble() -> Hsla {
    foreground().alpha(if dark() { 0.13 } else { 0.075 })
}

/// A hairline inside a card, between its rows.
pub fn hairline() -> Hsla {
    border_subtle().opacity(0.6)
}

/// The chosen segment of a track, raised off it: the card surface in the
/// light appearance, a stronger wash in the dark one, where a card would sink
/// into the track instead.
pub fn raised() -> Hsla {
    if dark() {
        foreground().alpha(0.14)
    } else {
        background_card()
    }
}

/// The pane a control floats in over the conversation.
///
/// A plain translucent fill, with no border: the shape is the shadow's job, and
/// an outline over a message reads as a box drawn on the page rather than a
/// surface hovering above it.
pub fn floating_pane() -> Hsla {
    // The card tone at 72%: enough of the message under it shows through that
    // the bar reads as glass, not so much that a glyph competes with it.
    background_card().alpha(0.72)
}

/// The pane a control floats in, with nothing showing through it.
///
/// [`floating_pane`] is glass, and is meant to be read against whatever scrolls
/// under it. A control that must stay legible over a busy transcript — the
/// bar's own buttons, whose glyphs sit over running prose — takes this instead,
/// so nothing is ever drawn behind the mark.
pub fn floating_pane_solid() -> Hsla {
    background_card()
}

/// The shadow a floating pane casts.
///
/// Soft, wide, and low: the pane has no border to say where it ends, so the
/// shadow has to lift it off the page on its own. Two layers — a tight one for
/// the edge, a wide one for the depth — which is what reads as a surface rather
/// than a smudge.
pub fn floating_pane_shadow() -> [BoxShadow; 2] {
    // One soft drop, with the faintest contact shadow under it. A single wide
    // shadow alone leaves the pane looking like it floats a little too high;
    // two heavy ones make it look stamped on. The light palette needs far less
    // ink than the dark one to read at all.
    let drop = hsla(0.0, 0.0, 0.0, if dark() { 0.34 } else { 0.10 });
    let contact = hsla(0.0, 0.0, 0.0, if dark() { 0.20 } else { 0.05 });
    [
        BoxShadow {
            offset_x: Points::ZERO,
            offset_y: Points(1.0),
            blur: Points(2.0),
            spread: Points(0.0),
            color: contact.into(),
            inset: false,
        },
        BoxShadow {
            offset_x: Points::ZERO,
            offset_y: Points(4.0),
            blur: Points(14.0),
            spread: Points(-1.0),
            color: drop.into(),
            inset: false,
        },
    ]
}

/// The scrim under a floating menu.
pub fn menu_scrim() -> Hsla {
    backdrop(if dark() { 0.5 } else { 0.28 })
}

/// A tint of `color` behind a mark or a line that carries its meaning.
pub fn tint(color: Hsla) -> Hsla {
    color.opacity(if dark() { 0.2 } else { 0.12 })
}

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
///
/// The desktop's own size (`icons/*.svg` at 14 in its sidebar rows), so the
/// two clients draw the same marks at the same weight.
pub const SIDEBAR_META_ICON: f32 = 14.0;
/// The round control at the end of a project or worktree row.
pub const SIDEBAR_ROW_ACTION: f32 = 28.0;
/// The mark a row wears while sessions are being selected.
pub const SIDEBAR_SELECTION_MARK: f32 = 20.0;
/// The floor of a tree row's height.
pub const SIDEBAR_ROW_HEIGHT: f32 = 46.0;

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

/// The plate of the session the drawer has open.
///
/// The accent alone is too quiet against the page: in the light palette it is
/// barely a shade off the sidebar, and the open row read as no row at all.
/// The desktop moves it away from the page — up in the dark palette, down in
/// the light one — so the chosen row is the one the eye lands on. The steps
/// are the desktop's own, so the two clients pick the same row out of a list.
pub fn sidebar_open_row_bg() -> Hsla {
    let accent = color_token("sidebar-accent");
    if dark() {
        accent.lighten(0.24)
    } else {
        accent.darken(0.04)
    }
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

/// A well on the sessions page: the search field, a round control.
pub fn sidebar_well() -> Hsla {
    fill()
}

/// A hairline on the sessions page.
pub fn sidebar_hairline() -> Hsla {
    hairline()
}

/// The wash a pressed row or control takes.
pub fn row_pressed_bg() -> Hsla {
    let luminance = if dark() { 0.92 } else { 0.10 };
    hsla(0.0, 0.0, luminance, 0.11)
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

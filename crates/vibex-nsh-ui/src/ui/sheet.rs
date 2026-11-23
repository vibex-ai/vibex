//! The bottom sheet every page pulls up, and the pieces it is built from.
//!
//! A sheet is the same object whichever page raises it: one grabber, one
//! scrim, one rounded lip on the page's own color, and one header with the way
//! out. What it holds are cards, so a sheet reads as a short page rather than
//! a list pinned to the bottom edge. Pages hand over a title and their cards;
//! nothing here knows what the rows do.

use nsh_base::{SheetEvent, button, sheet};
use nsh_core::prelude::*;

use super::surface;
use crate::{copy, theme};

/// The header of a sheet: its title and the control that closes it.
pub const SHEET_HEADER_HEIGHT: f32 = 52.0;

/// A sheet rising from the bottom edge, with the phone's grabber, scrim and
/// lip. `open` is read reactively; `on_close` hears every way out (the scrim,
/// a pull, the back key).
pub fn bottom_sheet(
    open: impl Fn() -> bool + 'static,
    on_close: impl Fn() + 'static,
    content: impl IntoElement,
) -> impl IntoElement {
    sheet()
        .open(move || Some(open()))
        .close_on_swipe(true)
        .grabber(true)
        .grabber_style(|style| {
            style
                .self_center()
                .w(px(theme::SHEET_GRABBER_WIDTH))
                .h(px(theme::SHEET_GRABBER_HEIGHT))
                .mt(px(theme::SPACING_SM))
                .mb(px(theme::SPACING_XS))
                .rounded(px(theme::SHEET_GRABBER_HEIGHT / 2.0))
                .bg(theme::muted_foreground().opacity(0.4))
        })
        .backdrop_style(|style| {
            style.bg(nsh_core::color::black().opacity(theme::SHEET_BACKDROP_OPACITY))
        })
        .popup_style(|style| {
            style
                .rounded_t(px(theme::RADIUS_SHEET))
                .bg(theme::page_adaptive)
        })
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(false, _) = event {
                on_close();
            }
        })
        .child(content)
}

/// The header of a sheet: what it is, and the one control that closes it.
pub fn sheet_header(
    title: impl IntoReactive<SharedString>,
    on_close: impl Fn() + 'static,
) -> impl IntoElement {
    view()
        .w_full()
        .h(px(SHEET_HEADER_HEIGHT))
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .pl(px(theme::SPACING_XL))
        .pr(px(theme::SPACING_MD))
        .child(
            text(title)
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::font_heading()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::foreground),
        )
        .child(
            button()
                .size(px(32.0))
                .flex_shrink_0()
                .rounded_full()
                .bg(theme::fill)
                .items_center()
                .justify_center()
                .hit_slop(px(6.0))
                .active(|style| style.opacity(0.6))
                .accessibility_label(copy::text("Close", "关闭", "關閉"))
                .on_press(on_close)
                .child(svg(icon!("x")).size(px(14.0)).text_color(theme::text_secondary)),
        )
}

/// The column a sheet's cards stack in, clear of the home indicator.
pub fn sheet_body() -> View {
    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_MD))
        .px(px(theme::SPACING_MD))
        .pt(px(theme::SPACING_XS))
        .pb_safe_area_plus(px(theme::SPACING_LG))
}

/// A card of a sheet: the surface its rows share.
pub fn sheet_card() -> View {
    surface::card()
}

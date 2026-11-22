//! The bottom sheet every page pulls up, and the pieces it is built from.
//!
//! A sheet is the same object whichever page raises it: one grabber, one
//! scrim, one rounded lip and one header with the way out. Pages hand over a
//! title and their rows; nothing here knows what the rows do.

use nsh_base::{ButtonElement, SheetEvent, button, sheet};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::{copy, theme};

/// The header of a sheet: its title and the control that closes it.
pub const SHEET_HEADER_HEIGHT: f32 = theme::DRAWER_HEADER_HEIGHT;

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
                .bg(theme::muted_foreground().opacity(0.55))
        })
        .backdrop_style(|style| {
            style.bg(nsh_core::color::black().opacity(theme::SHEET_BACKDROP_OPACITY))
        })
        .popup_style(|style| {
            style
                .rounded_t(px(theme::RADIUS_SHEET))
                .border_t_1()
                .border_color(theme::border_adaptive)
                .bg(theme::card_adaptive)
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
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .pl(px(theme::SPACING_MD))
        .pr(px(theme::SPACING_XS))
        .child(
            text(title)
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::font_heading()))
                .text_color(theme::foreground),
        )
        .child(
            button()
                .size(px(theme::TOUCH_TARGET))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .active(|style| style.opacity(0.65))
                .accessibility_label(copy::text("Close", "关闭", "關閉"))
                .on_press(on_close)
                .child(
                    svg(icon!("x"))
                        .w(px(theme::ICON_SM))
                        .h(px(theme::ICON_SM))
                        .text_color(theme::text_muted),
                ),
        )
}

/// One command of a sheet: a glyph and what it does, a touch target tall.
pub fn sheet_entry(
    icon: ImageSrc,
    label: impl IntoReactive<SharedString>,
    danger: bool,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    let ink: fn() -> Hsla = if danger {
        theme::accent_red
    } else {
        theme::foreground
    };
    let glyph: fn() -> Hsla = if danger {
        theme::accent_red
    } else {
        theme::text_muted
    };
    button()
        .w_full()
        .h(px(theme::TOUCH_TARGET))
        .px(px(theme::SPACING_MD))
        .rounded(px(theme::RADIUS_CONTROL))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .on_press(on_press)
        .child(
            svg(icon)
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(glyph),
        )
        .child(
            text(label)
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::font_body()))
                .text_color(ink),
        )
}

/// The column a sheet's commands stack in.
pub fn sheet_body() -> View {
    view()
        .w_full()
        .flex_col()
        .gap(px(2.0))
        .p(px(theme::SPACING_SM))
        .pb_safe_area_plus(px(theme::SPACING_SM))
}

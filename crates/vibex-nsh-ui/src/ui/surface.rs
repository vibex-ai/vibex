//! The surfaces a page is built from.
//!
//! A page is a column of a few kinds of object: a round control, a quiet
//! glyph, an icon tile at the head of a row, a card of rows, a pill, a
//! segmented track. Each is drawn once here, so a page composes them and never
//! restates a radius or a fill.

use nsh_base::{ButtonElement, button};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::theme;

/// The inset of a card's rows, on both sides.
pub const ROW_INSET: f32 = 14.0;

/// A round control in a well: a page's own chrome.
pub fn round_button(
    icon: ImageSrc,
    label: &'static str,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    round_button_sized(icon, label, on_press, theme::ICON_MD)
}

/// [`round_button`] with a glyph of its own size.
///
/// A duotone mark — a quarter-opacity wash behind a thin outline — reads
/// smaller than a solid stroke in the same box, so a control wearing one asks
/// for a size the others do not.
pub fn round_button_sized(
    icon: ImageSrc,
    label: &'static str,
    on_press: impl Fn() + 'static,
    glyph: f32,
) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .size(px(theme::ROUND_CONTROL))
        .flex_shrink_0()
        .rounded_full()
        .bg(theme::fill)
        .items_center()
        .justify_center()
        .active(|style| style.opacity(0.6))
        .disabled_style(|style| style.opacity(0.4))
        .child(svg(icon).size(px(glyph)).text_color(theme::foreground))
}

/// A glyph alone, at the size of the round controls beside it.
///
/// The same touch target as [`round_button`] without the disc behind it, for a
/// mark that already reads as a shape of its own — a duotone bubble is a
/// drawing, and a filled circle under it only framed the frame.
pub fn bare_icon_button(
    icon: ImageSrc,
    label: &'static str,
    on_press: impl Fn() + 'static,
    glyph: f32,
    ink: fn() -> Hsla,
) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .size(px(theme::ROUND_CONTROL))
        .flex_shrink_0()
        .rounded_full()
        .items_center()
        .justify_center()
        .active(|style| style.opacity(0.6))
        .disabled_style(|style| style.opacity(0.4))
        .child(svg(icon).size(px(glyph)).text_color(ink))
}

/// A quiet icon control: a glyph with a touch target and no fill of its own.
pub fn ghost_button(
    icon: ImageSrc,
    label: &'static str,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .size(px(theme::COMPACT_CONTROL))
        .flex_shrink_0()
        .rounded_full()
        .items_center()
        .justify_center()
        .hit_slop(px(5.0))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .disabled_style(|style| style.opacity(0.4))
        .child(
            svg(icon)
                .size(px(theme::ICON_MD))
                .text_color(theme::text_secondary),
        )
}

/// An icon tile: the mark at the head of a row.
pub fn tile(icon: ImageSrc) -> View {
    tile_in(icon, theme::text_secondary, theme::fill)
}

fn tile_in(icon: ImageSrc, ink: fn() -> Hsla, fill: fn() -> Hsla) -> View {
    view()
        .size(px(theme::TILE_SIZE))
        .flex_shrink_0()
        .rounded(px(theme::RADIUS_TILE))
        .bg(fill)
        .items_center()
        .justify_center()
        .child(svg(icon).size(px(theme::TILE_ICON)).text_color(ink))
}

/// A card: one raised surface that rows, a field or a note sit on.
pub fn card() -> View {
    view()
        .w_full()
        .flex_col()
        .rounded(px(theme::RADIUS_GROUP))
        .bg(theme::block_surface)
        .border_1()
        .border_color(theme::hairline)
        .overflow_hidden()
}

fn row_frame(on_press: impl Fn() + 'static) -> ButtonElement {
    button()
        .on_press(on_press)
        .w_full()
        .min_h(px(theme::ROW_HEIGHT))
        .px(px(ROW_INSET))
        .py(px(theme::SPACING_SM))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_MD))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .disabled_style(|style| style.opacity(0.4))
}

fn row_title(title: impl IntoReactive<SharedString>, ink: fn() -> Hsla) -> impl IntoElement {
    text(title)
        .truncate()
        .text_size(px(theme::font_body()))
        .text_color(ink)
}

/// One row of a card: a tile and what the row does. What trails the title is
/// appended by the caller.
pub fn row(
    icon: ImageSrc,
    title: impl IntoReactive<SharedString>,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    row_frame(on_press).child(tile(icon)).child(
        view()
            .flex_1()
            .min_w(px(0.0))
            .child(row_title(title, theme::foreground)),
    )
}

/// A row with a line under its title saying what it is for.
pub fn row_captioned(
    icon: ImageSrc,
    title: impl IntoReactive<SharedString>,
    caption: impl IntoReactive<SharedString>,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    row_frame(on_press).child(tile(icon)).child(
        view()
            .flex_1()
            .min_w(px(0.0))
            .flex_col()
            .gap(px(1.0))
            .child(row_title(title, theme::foreground))
            .child(
                text(caption)
                    .truncate()
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted),
            ),
    )
}

/// The hairline between two rows of a card. It starts where the titles do, so
/// the tiles read as one column.
pub fn row_divider() -> View {
    view()
        .h(px(1.0))
        .flex_shrink_0()
        .ml(px(ROW_INSET + theme::TILE_SIZE + theme::SPACING_MD))
        .bg(theme::hairline)
}

/// The mark at the end of a row that leads somewhere.
pub fn chevron() -> impl IntoElement {
    svg(icon!("chevron-right"))
        .size(px(14.0))
        .flex_shrink_0()
        .text_color(theme::text_muted)
}

/// The name over a group of rows.
pub fn group_label(label: impl IntoReactive<SharedString>) -> impl IntoElement {
    text(label)
        .px(px(theme::SPACING_XS))
        .text_size(px(theme::font_micro()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_muted)
}

/// One of several choices as a pill: the chosen one is filled with the ink.
pub fn choice_pill(
    label: impl Fn() -> String + 'static,
    selected: impl Fn() -> bool + Copy + 'static,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .on_press(on_press)
        .h(px(theme::SEGMENT_HEIGHT))
        .min_w(px(0.0))
        .px(px(ROW_INSET))
        .rounded_full()
        .items_center()
        .justify_center()
        .bg(move || {
            if selected() {
                theme::action_primary()
            } else {
                theme::fill()
            }
        })
        .active(|style| style.opacity(0.7))
        .disabled_style(|style| style.opacity(0.42))
        .child(
            text(label)
                .truncate()
                .text_size(px(theme::font_caption()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || {
                    if selected() {
                        theme::action_primary_label()
                    } else {
                        theme::text_secondary()
                    }
                }),
        )
}

/// A switch drawn as a pill beside a glyph: on is filled with the ink.
pub fn toggle_pill(
    icon: ImageSrc,
    label: &'static str,
    selected: impl Fn() -> bool + Copy + 'static,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .h(px(theme::CHIP_HEIGHT))
        .px(px(theme::SPACING_MD))
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded_full()
        .bg(move || {
            if selected() {
                theme::action_primary()
            } else {
                theme::fill()
            }
        })
        .active(|style| style.opacity(0.7))
        .disabled_style(|style| style.opacity(0.42))
        .child(svg(icon).size(px(14.0)).text_color(move || {
            if selected() {
                theme::action_primary_label()
            } else {
                theme::text_secondary()
            }
        }))
        .child(
            text(label)
                .text_size(px(theme::font_micro()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || {
                    if selected() {
                        theme::action_primary_label()
                    } else {
                        theme::text_secondary()
                    }
                }),
        )
}

/// The track of a segmented control.
pub fn segment_track() -> View {
    view()
        .h(px(theme::SEGMENT_HEIGHT))
        .p(px(3.0))
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .rounded(px(theme::RADIUS_CHIP))
        .bg(theme::fill)
}

/// One segment of a track: the chosen one is raised off it. What trails the
/// label (a count, a badge) is appended by the caller.
pub fn segment(
    label: &'static str,
    selected: impl Fn() -> bool + Copy + 'static,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        // A segment takes the room its own label needs before the rest is
        // shared, so one that carries a count beside its name keeps both.
        .flex_auto()
        .min_w(px(0.0))
        .h_full()
        .px(px(6.0))
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACING_XS))
        .rounded(px(theme::RADIUS_CHIP - 3.0))
        .style_when(selected, |style| style.bg(theme::raised()).shadow_xs())
        .active(|style| style.opacity(0.7))
        .child(
            text(label)
                .truncate()
                .text_size(px(theme::font_caption()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || {
                    if selected() {
                        theme::foreground()
                    } else {
                        theme::text_muted()
                    }
                }),
        )
}

/// A pill a field is written in: a glyph, the field, and what trails it.
pub fn field_pill() -> View {
    view()
        .h(px(theme::ROUND_CONTROL))
        .pl(px(theme::SPACING_MD))
        .pr(px(theme::SPACING_XS))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .rounded(px(theme::RADIUS_CHIP))
        .bg(theme::fill)
}

/// The glyph at the head of a field pill.
pub fn field_glyph(icon: ImageSrc) -> impl IntoElement {
    svg(icon)
        .size(px(theme::ICON_SM))
        .flex_shrink_0()
        .text_color(theme::text_muted)
}

/// The control that empties a field pill.
pub fn field_clear(label: &'static str, on_press: impl Fn() + 'static) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .size(px(32.0))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_full()
        .active(|style| style.opacity(0.6))
        .child(svg(icon!("x")).size(px(14.0)).text_color(theme::text_muted))
}

/// The mark of a row that can be chosen among others: an empty ring, a filled
/// one with a check, or with a dash for a scope only partly chosen.
pub fn check_mark(mark: impl Fn() -> Option<bool> + Copy + 'static) -> View {
    view()
        .size(px(theme::SIDEBAR_SELECTION_MARK))
        .flex_shrink_0()
        .rounded_full()
        .border(px(1.5))
        .border_color(move || {
            if mark().is_some() {
                theme::primary()
            } else {
                theme::text_muted()
            }
        })
        .bg(move || {
            if mark().is_some() {
                theme::primary()
            } else {
                theme::primary().opacity(0.0)
            }
        })
        .items_center()
        .justify_center()
        .child(when(
            move || mark() == Some(true),
            || {
                svg(icon!("check"))
                    .size(px(13.0))
                    .text_color(theme::primary_foreground)
            },
        ))
        .child(when(
            move || mark() == Some(false),
            || {
                view()
                    .w(px(9.0))
                    .h(px(2.0))
                    .rounded_full()
                    .bg(theme::primary_foreground)
            },
        ))
}

/// A small mark of state: a dot in the color that state is told by.
pub fn status_dot(color: impl Fn() -> Hsla + 'static) -> View {
    view()
        .size(px(theme::SIDEBAR_STATUS_DOT))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
}

/// A few words on a tint of the color that carries their meaning.
pub fn badge(label: impl IntoReactive<SharedString>, color: fn() -> Hsla) -> View {
    view()
        .h(px(20.0))
        .min_w(px(20.0))
        .px(px(6.0))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(move || theme::tint(color()))
        .child(
            text(label)
                .text_size(px(theme::font_hint()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(color),
        )
}

/// What a page says where its content will be: a mark, a line, and a quieter
/// line under it.
pub fn empty_state(
    icon: ImageSrc,
    title: impl IntoReactive<SharedString>,
    detail: impl IntoReactive<SharedString>,
) -> View {
    view()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACING_SM))
        .px(px(theme::SPACING_XL * 2.0))
        .child(
            view()
                .size(px(52.0))
                .mb(px(theme::SPACING_XS))
                .rounded(px(theme::RADIUS_GROUP))
                .bg(theme::fill)
                .items_center()
                .justify_center()
                .child(svg(icon).size(px(24.0)).text_color(theme::text_muted)),
        )
        .child(
            text(title)
                .text_center()
                .text_size(px(theme::font_body()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_secondary),
        )
        .child(
            text(detail)
                .text_center()
                .text_size(px(theme::font_caption()))
                .line_height(theme::line_height_for(theme::font_caption()))
                .text_color(theme::text_muted),
        )
}

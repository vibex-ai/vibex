//! Pressable actions.
//!
//! `nsh_base`'s `button()` is an invisible box that carries focus, a role and
//! the press; these wrap it once with the phone's look. A screen picks a
//! variant, never a color: primary for the one action a page is for,
//! secondary for the alternatives, with an optional selected state.

use nsh_base::{ButtonElement, button};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::theme;

/// The page's main action: a filled row with an icon and a label.
pub fn primary(
    label: impl IntoReactive<SharedString>,
    icon: ImageSrc,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    action(label, icon, true, false, on_press)
}

/// An alternative action, aligned with [`primary`].
pub fn secondary(
    label: impl IntoReactive<SharedString>,
    icon: ImageSrc,
    selected: bool,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    action(label, icon, false, selected, on_press)
}

/// An alternative action drawn as a filled block with no outline.
///
/// Page-level actions sit directly on the page, so they take the same block
/// surface as a card instead of an outline that only reads on a card.
pub fn block(
    label: impl IntoReactive<SharedString>,
    icon: ImageSrc,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    let content: fn() -> Hsla = theme::text_secondary;
    button()
        .on_press(on_press)
        .h(px(theme::TOUCH_TARGET))
        .rounded(px(theme::RADIUS_CONTROL))
        .bg(theme::block_surface)
        .shadow_xs()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACING_SM))
        .active(|style| style.opacity(0.85))
        .focus_visible(|style| style.outline(px(2.0)).outline_color(theme::foreground))
        .child(
            svg(icon)
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(content),
        )
        .child(
            text(label)
                .text_size(px(theme::font_detail()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(content),
        )
}

/// An icon-only control floating on the page in its own circular block.
pub fn floating_icon(
    icon: ImageSrc,
    label: &'static str,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .size(px(theme::TOUCH_TARGET))
        .rounded(px(theme::TOUCH_TARGET / 2.0))
        .bg(theme::block_surface)
        .shadow_xs()
        .items_center()
        .justify_center()
        .active(|style| style.opacity(0.65))
        .child(
            svg(icon)
                .w(px(theme::ICON_MD))
                .h(px(theme::ICON_MD))
                .text_color(theme::foreground),
        )
}

/// The one action a card or a sheet is for: a labelled block in the ink.
pub fn solid(label: impl IntoReactive<SharedString>, on_press: impl Fn() + 'static) -> ButtonElement {
    labelled(label, theme::action_primary, theme::action_primary_label, on_press)
}

/// The alternative beside a [`solid`] action: the same block on a wash.
pub fn tonal(label: impl IntoReactive<SharedString>, on_press: impl Fn() + 'static) -> ButtonElement {
    labelled(label, theme::fill, theme::foreground, on_press)
}

/// A [`tonal`] action that removes or discards.
pub fn tonal_danger(
    label: impl IntoReactive<SharedString>,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    labelled(label, danger_fill, theme::accent_red, on_press)
}

fn danger_fill() -> Hsla {
    theme::tint(theme::accent_red())
}

fn labelled(
    label: impl IntoReactive<SharedString>,
    fill: fn() -> Hsla,
    ink: fn() -> Hsla,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .on_press(on_press)
        .h(px(theme::TOUCH_TARGET))
        .px(px(theme::SPACING_LG))
        .rounded(px(theme::RADIUS_BUTTON))
        .bg(fill)
        .items_center()
        .justify_center()
        .active(|style| style.opacity(0.75))
        .disabled_style(|style| style.opacity(0.4))
        .focus_visible(|style| style.outline(px(2.0)).outline_color(theme::foreground))
        .child(
            text(label)
                .truncate()
                .text_size(px(theme::font_detail()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ink),
        )
}

/// A quiet action written out in words.
pub fn link(label: impl IntoReactive<SharedString>, on_press: impl Fn() + 'static) -> ButtonElement {
    button()
        .on_press(on_press)
        .h(px(theme::CHIP_HEIGHT))
        .px(px(theme::SPACING_SM))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_CHIP))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .disabled_style(|style| style.opacity(0.4))
        .child(
            text(label)
                .text_size(px(theme::font_caption()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_secondary),
        )
}

fn action(
    label: impl IntoReactive<SharedString>,
    icon: ImageSrc,
    primary: bool,
    selected: bool,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    // Function pointers, not resolved colors: the style bindings follow the
    // appearance when the system switches, as the values are read again.
    let fill: fn() -> Hsla = if primary {
        theme::action_primary
    } else if selected {
        theme::background_card
    } else {
        theme::background_dim
    };
    let stroke: fn() -> Hsla = if primary {
        theme::action_primary
    } else if selected {
        theme::border_default
    } else {
        theme::border_subtle
    };
    let content: fn() -> Hsla = if primary {
        theme::action_primary_label
    } else {
        theme::text_secondary
    };
    button()
        .on_press(on_press)
        .h(px(theme::TOUCH_TARGET))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(stroke)
        .bg(fill)
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACING_SM))
        .active(|style| style.opacity(0.85))
        .focus_visible(|style| style.outline(px(2.0)).outline_color(theme::foreground))
        .child(
            svg(icon)
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(content),
        )
        .child(
            text(label)
                .text_size(px(theme::font_detail()))
                .font_weight(if primary {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(content),
        )
}

/// An icon-only button: a 44-point target, no frame, and a label screen
/// readers say (and tests select it by).
pub fn icon_button(
    icon: ImageSrc,
    label: &'static str,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .size(px(theme::TOUCH_TARGET))
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_CONTROL))
        .active(|style| style.opacity(0.6))
        .child(
            svg(icon)
                .w(px(theme::ICON_MD))
                .h(px(theme::ICON_MD))
                .text_color(theme::foreground),
        )
}

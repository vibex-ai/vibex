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
    label: &'static str,
    icon: ImageSrc,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    action(label, icon, true, false, on_press)
}

/// An alternative action, aligned with [`primary`].
pub fn secondary(
    label: &'static str,
    icon: ImageSrc,
    selected: bool,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    action(label, icon, false, selected, on_press)
}

fn action(
    label: &'static str,
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
                .text_size(px(theme::FONT_DETAIL))
                .font_weight(if primary {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(content),
        )
}

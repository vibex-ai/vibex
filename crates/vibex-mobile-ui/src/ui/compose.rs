//! The pieces the prompt card is built from.
//!
//! The card is the one surface the reader writes on, and it is the same card
//! whether the message opens a session or continues one: the field, and under
//! it one row of controls, with the way to add to the message on the leading
//! side and the action that sends it on the trailing one.

use nsh_base::{ButtonElement, Input, button};
use nsh_core::{element::ImageSrc, prelude::*};

use super::field;
use crate::{copy, theme};

/// The prompt hint shared by both an open session and a new session.
pub fn placeholder() -> &'static str {
    copy::text(
        "/ commands @files & refs $skills",
        "/ 命令短语 @文件委派引用 $技能",
        "/ 命令短語 @檔案委派參照 $技能",
    )
}

/// The card itself: rounder than any other surface, a borderless filled block
/// lifted cleanly off the page tone.
pub fn card() -> View {
    view()
        .w_full()
        .relative()
        .flex_col()
        .rounded(px(theme::RADIUS_COMPOSER))
        .bg(theme::composer_surface)
        .shadow_sm()
        .p(px(theme::SPACING_SM))
        .test_id("composer-card")
}

/// The field: starts as a single breathing line and grows with what is written.
pub fn field(
    value: Signal<String>,
    placeholder: impl IntoReactive<String>,
    on_change: impl Fn(&str) + 'static,
) -> Input {
    field::text_field_flat_with(value, placeholder, on_change)
        .multiline(true)
        .h(nsh_core::geometry::Length::Auto)
        .max_h(px(132.0))
        .px(px(theme::SPACING_SM))
        .pt(px(6.0))
        .pb(px(8.0))
        .text_size(px(theme::font_composer()))
}

/// The row of controls under the field.
pub fn controls() -> View {
    view()
        .w_full()
        .h(px(theme::ROUND_CONTROL))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_XS))
}

/// The control that adds to the message.
pub fn add_button(label: &'static str, on_press: impl Fn() + 'static) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .size(px(theme::COMPOSER_OPTION_SIZE))
        .flex_shrink_0()
        .rounded_full()
        .bg(theme::fill)
        .items_center()
        .justify_center()
        .active(|style| style.opacity(0.6))
        .disabled_style(|style| style.opacity(0.38))
        .child(
            svg(icon!("plus"))
                .size(px(theme::ICON_MD))
                .text_color(theme::foreground),
        )
}

/// A value the message is sent with, and the way to change it.
pub fn pill(
    icon: ImageSrc,
    value: impl IntoReactive<SharedString>,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    pill_in(icon, value, theme::text_secondary, on_press)
}

/// A [`pill`] whose value is written in its own ink.
pub fn pill_in(
    icon: ImageSrc,
    value: impl IntoReactive<SharedString>,
    ink: impl Fn() -> Hsla + 'static,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .on_press(on_press)
        .h(px(theme::CHIP_HEIGHT))
        .min_w(px(0.0))
        .flex_shrink(1.0)
        .pl(px(10.0))
        .pr(px(theme::SPACING_SM))
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded_full()
        .bg(theme::fill)
        .active(|style| style.opacity(0.6))
        .disabled_style(|style| style.opacity(0.42))
        .child(
            svg(icon)
                .size(px(14.0))
                .flex_shrink_0()
                .text_color(theme::text_secondary),
        )
        .child(
            text(value)
                .min_w(px(0.0))
                .flex_shrink(1.0)
                .truncate()
                .text_size(px(theme::font_caption()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(ink),
        )
        .child(
            svg(icon!("chevron-down"))
                .size(px(11.0))
                .flex_shrink_0()
                .text_color(theme::text_muted),
        )
}

/// The unified Agent & model trigger on the composer's control row.
///
/// Matches desktop's runtime trigger: a brand mark on the left, the model name
/// that truncates when width is tight, an optional non-truncating reasoning
/// depth on its right, and the disclosure chevron.
pub fn runtime_pill(
    icon: impl Fn() -> ImageSrc + 'static,
    value: impl IntoReactive<SharedString>,
    detail: impl Fn() -> Option<SharedString> + Clone + 'static,
    ink: impl Fn() -> Hsla + 'static,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    let show_detail = detail.clone();
    button()
        .on_press(on_press)
        .h(px(theme::CHIP_HEIGHT))
        .min_w(px(0.0))
        .flex_shrink(1.0)
        .pl(px(10.0))
        .pr(px(theme::SPACING_SM))
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded_full()
        .bg(theme::fill)
        .active(|style| style.opacity(0.6))
        .disabled_style(|style| style.opacity(0.42))
        .child(dynamic(move || {
            svg(icon())
                .size(px(14.0))
                .flex_shrink_0()
                .text_color(theme::text_secondary)
        }))
        .child(
            text(value)
                .min_w(px(0.0))
                .flex_shrink(1.0)
                .truncate()
                .text_size(px(theme::font_caption()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(ink),
        )
        .child(when(
            move || show_detail().is_some(),
            move || {
                let detail = detail.clone();
                text(move || detail().unwrap_or_default())
                    .flex_shrink_0()
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted)
            },
        ))
        .child(
            svg(icon!("chevron-down"))
                .size(px(11.0))
                .flex_shrink_0()
                .text_color(theme::text_muted),
        )
}

/// An icon control on the row: quiet until what it stands for is set.
pub fn option(
    icon: ImageSrc,
    label: &'static str,
    set: impl Fn() -> bool + 'static,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .size(px(theme::COMPOSER_OPTION_SIZE))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_full()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .disabled_style(|style| style.opacity(0.42))
        .child(svg(icon).size(px(theme::ICON_MD)).text_color(move || {
            if set() {
                theme::foreground()
            } else {
                theme::text_muted()
            }
        }))
}

/// The action that sends: filled with the ink once there is something to
/// send, a quiet well until then.
pub fn send_button(
    icon: ImageSrc,
    label: &'static str,
    ready: impl Fn() -> bool + Copy + 'static,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .accessibility_label(label)
        .on_press(on_press)
        .size(px(theme::COMPOSER_SEND_SIZE))
        .flex_shrink_0()
        .rounded_full()
        .items_center()
        .justify_center()
        .bg(move || {
            if ready() {
                theme::action_primary()
            } else {
                theme::fill_strong()
            }
        })
        .active(|style| style.opacity(0.75))
        .disabled(move || !ready())
        .child(svg(icon).size(px(theme::ICON_MD)).text_color(move || {
            if ready() {
                theme::action_primary_label()
            } else {
                theme::text_muted()
            }
        }))
}

/// Something attached to the message. Pressing it takes it off again.
pub fn chip(
    label: impl IntoReactive<SharedString>,
    on_remove: impl Fn() + 'static,
) -> ButtonElement {
    button()
        .on_press(on_remove)
        .h(px(30.0))
        .max_w(relative(1.0))
        .pl(px(10.0))
        .pr(px(theme::SPACING_SM))
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .rounded_full()
        .bg(theme::fill)
        .active(|style| style.opacity(0.6))
        .child(
            svg(icon!("paperclip"))
                .size(px(13.0))
                .flex_shrink_0()
                .text_color(theme::text_muted),
        )
        .child(
            text(label)
                .min_w(px(0.0))
                .flex_shrink(1.0)
                .truncate()
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_secondary),
        )
        .child(
            svg(icon!("x"))
                .size(px(12.0))
                .flex_shrink_0()
                .text_color(theme::text_muted),
        )
}

/// The row attached things wrap in, above the field.
pub fn chips() -> View {
    view()
        .w_full()
        .flex_row()
        .flex_wrap()
        .gap(px(theme::SPACING_XS))
        .px(px(theme::SPACING_XS))
        .pt(px(theme::SPACING_XS))
}

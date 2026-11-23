//! Text fields.

use nsh_base::{Input, input};
use nsh_core::prelude::*;

use crate::theme;

/// A single-line field on the phone's control ladder, without a frame of its
/// own: the pill, the well or the card around it draws one.
///
/// The field is controlled: the owner keeps the text in `value` and
/// `on_change` writes it back. The platform draws the caret, selection,
/// autocorrection and keyboard, so the app does not.
pub fn text_field_flat(value: Signal<String>, placeholder: impl IntoReactive<String>) -> Input {
    text_field_flat_with(value, placeholder, |_text: &str| {})
}

/// The same field, whose owner also hears every edit (a composer that opens
/// its command palette on `/`).
pub fn text_field_flat_with(
    value: Signal<String>,
    placeholder: impl IntoReactive<String>,
    on_change: impl Fn(&str) + 'static,
) -> Input {
    input()
        .value(move || value.get())
        .on_change(move |text: &str| {
            value.set(text.to_owned());
            on_change(text);
        })
        .placeholder(placeholder)
        .placeholder_color(theme::muted_foreground)
        .caret_color(theme::primary)
        .h(px(theme::TOUCH_TARGET))
        .px(px(theme::SPACING_MD))
        .text_size(px(theme::font_body()))
        .text_color(theme::foreground)
}

/// A field with neither frame nor padding, for a pill that draws both around
/// the field and the glyphs beside it.
pub fn text_field_bare(value: Signal<String>, placeholder: impl IntoReactive<String>) -> Input {
    text_field_bare_with(value, placeholder, |_text: &str| {})
}

/// A bare field whose owner also hears every edit.
pub fn text_field_bare_with(
    value: Signal<String>,
    placeholder: impl IntoReactive<String>,
    on_change: impl Fn(&str) + 'static,
) -> Input {
    input()
        .value(move || value.get())
        .on_change(move |text: &str| {
            value.set(text.to_owned());
            on_change(text);
        })
        .placeholder(placeholder)
        .placeholder_color(theme::muted_foreground)
        .caret_color(theme::primary)
        .text_size(px(theme::font_detail()))
        .text_color(theme::foreground)
}

/// A field in a well: the one field a sheet or a card asks for.
pub fn text_field_well(value: Signal<String>, placeholder: impl IntoReactive<String>) -> Input {
    text_field_flat(value, placeholder)
        .h(px(48.0))
        .px(px(theme::SPACING_LG))
        .rounded(px(theme::RADIUS_BUTTON))
        .bg(theme::fill)
}

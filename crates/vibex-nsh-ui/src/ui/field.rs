//! Text fields.

use nsh_base::{Input, input};
use nsh_core::prelude::*;

use crate::theme;

/// A single-line field on the phone's control ladder.
///
/// The field is controlled: the owner keeps the text in `value` and
/// `on_change` writes it back. The platform draws the caret, selection,
/// autocorrection and keyboard, so the app does not.
pub fn text_field(value: Signal<String>, placeholder: impl IntoReactive<String>) -> Input {
    text_field_with(value, placeholder, |_text: &str| {})
}

/// A field whose owner also hears every edit (a composer that opens its
/// command palette on `/`).
pub fn text_field_with(
    value: Signal<String>,
    placeholder: impl IntoReactive<String>,
    on_change: impl Fn(&str) + 'static,
) -> Input {
    framed(text_field_flat_with(value, placeholder, on_change))
}

/// The same field without its own frame, for a row that draws one border
/// around the field and the controls beside it.
pub fn text_field_flat(value: Signal<String>, placeholder: impl IntoReactive<String>) -> Input {
    text_field_flat_with(value, placeholder, |_text: &str| {})
}

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

fn framed(field: Input) -> Input {
    field
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_default)
        .focus(|style| style.border_color(theme::foreground))
}

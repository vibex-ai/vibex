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
    input()
        .value(move || value.get())
        .on_change(move |text: &str| value.set(text.to_owned()))
        .placeholder(placeholder)
        .placeholder_color(theme::muted_foreground)
        .caret_color(theme::primary)
        .h(px(theme::TOUCH_TARGET))
        .px(px(theme::SPACING_MD))
        .text_size(px(theme::FONT_BODY))
        .text_color(theme::foreground)
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_default)
        .focus(|style| style.border_color(theme::foreground))
}

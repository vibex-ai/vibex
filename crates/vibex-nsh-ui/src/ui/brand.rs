//! The Vibex wordmark and the small marks a page shows.

use nsh_core::{element::ImageSrc, prelude::*};

use crate::theme;

/// The mark alone, drawn at `size` points in the current text color.
///
/// The SVG is inlined so it shows in the first frame and needs no asset
/// registration; the same file the GPUI client loads is copied into this
/// crate's `assets/icons/` while the two clients run side by side.
pub fn mark(size: f32) -> impl IntoElement {
    svg(ImageSrc::svg_markup(include_str!(
        "../../assets/icons/vibex-mark.svg"
    )))
    .w(px(size))
    .h(px(size))
    .flex_shrink_0()
    .text_color(theme::foreground)
}

/// The launch lockup: the mark and the product word, side by side, as the
/// pairing and connecting pages show it.
pub fn wordmark() -> impl IntoElement {
    let size = theme::FONT_WORDMARK;
    view()
        .flex_row()
        .items_center()
        // The mark carries its own bearing in the SVG; the one-point gap is
        // the spacing the phone's lockup keeps between the two shapes. The GPUI
        // client also nudges the mark against the label's cap height, which
        // depends on the text engine: the platform's metrics here are the ones
        // a device shows, so the lockup is centered and left at that.
        .gap(px(1.0))
        .child(mark(size))
        .child(
            text("ibex")
                .text_size(px(size))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::foreground),
        )
}

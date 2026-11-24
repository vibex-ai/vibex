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
    wordmark_at(theme::font_wordmark())
}

/// The same lockup at `size`, for a surface that is not a launch page.
///
/// The mark's geometry is scaled from the label, so the two stay one word on
/// one baseline at every step the caller asks for.
pub fn wordmark_at(size: f32) -> impl IntoElement {
    let mark = theme::wordmark_mark(size);
    view()
        .flex_row()
        .items_center()
        // The GPUI client's lockup: a one-point gap, the mark a little wider
        // than the label is tall, its point on the label's baseline.
        .gap(px(1.0))
        .child(
            svg(ImageSrc::svg_markup(include_str!(
                "../../assets/icons/vibex-mark.svg"
            )))
            .w(px(mark.width))
            .h(px(mark.height))
            .mt(px(mark.margin_top))
            .relative()
            .top(px(mark.top_offset))
            .flex_shrink_0()
            .text_color(theme::foreground),
        )
        .child(
            text("ibex")
                .text_size(px(size))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::foreground),
        )
}

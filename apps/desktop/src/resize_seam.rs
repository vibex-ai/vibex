//! Shared styling for drag-resizable pane seams.
//!
//! Contract: every resizable boundary reads as the standard 1px `border`
//! hairline at rest. Hover brightens the seam by a fixed step — never a hue
//! shift — over the shared 150ms hover fade, and the stronger line melts back
//! into the resting border toward both ends of the seam. An active drag pins
//! the highlight until the pointer is released.

use gpui::prelude::FluentBuilder as _;
use gpui::{Div, Hsla, ParentElement, Styled, div, linear_color_stop, linear_gradient};

use crate::motion::hover_blend;

/// Hover/drag step above the resting `border` hairline (dark: white 10% → 17%).
const HIGHLIGHT_STEP: f32 = 1.7;

/// Stable hover-fade key for one seam's highlight line. Pair the key with
/// [`crate::motion::hover_listener`] on the seam's hitbox so the fade runs.
pub fn seam_hover_key(id: &'static str) -> String {
    crate::motion::hover_key("pane-resize-seam", id)
}

/// The 1px seam line, sized and positioned by the caller along the boundary (a
/// 1px-wide column for a vertical seam, a 1px-tall row for a horizontal one).
///
/// `rest_visible` paints the resting `border` hairline in the line itself; set
/// it to false when the adjacent surface paints its own hairline and the line
/// should only fade in over it. `pinned` holds the hover strength while a drag
/// keeps the seam active.
pub fn seam_line(
    key: &str,
    border: Hsla,
    rest_visible: bool,
    pinned: bool,
    horizontal: bool,
) -> Div {
    let highlight = border.opacity(HIGHLIGHT_STEP);
    let overlay = if pinned {
        highlight
    } else {
        hover_blend(key, border.opacity(0.0), highlight)
    };
    let clear = overlay.opacity(0.0);
    let (track, angle) = if horizontal {
        (div().flex().flex_row(), 90.0)
    } else {
        (div().flex().flex_col(), 180.0)
    };
    div().when(rest_visible, |line| line.bg(border)).child(
        track
            .absolute()
            .inset_0()
            .child(div().flex_1().bg(linear_gradient(
                angle,
                linear_color_stop(clear, 0.0),
                linear_color_stop(overlay, 1.0),
            )))
            .child(div().flex_1().bg(linear_gradient(
                angle,
                linear_color_stop(overlay, 0.0),
                linear_color_stop(clear, 1.0),
            ))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::hsla;

    #[test]
    fn highlight_steps_alpha_without_shifting_hue() {
        let border = hsla(0.6, 0.2, 0.5, 0.1);
        let highlight = highlight(border);
        assert!((highlight.a - border.a * HIGHLIGHT_STEP).abs() < 1e-6);
        assert_eq!(
            (highlight.h, highlight.s, highlight.l),
            (border.h, border.s, border.l)
        );
    }
}

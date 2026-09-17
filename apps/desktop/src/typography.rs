//! Type scale for the workbench's floating surfaces.
//!
//! Control typography is pinned to a rem ladder rather than raw pixels so the
//! whole UI tracks the window's `rem_size` (accessibility text scaling). A
//! hard-coded `px(13.0)` ignores that setting; `ui_rems(13.0)` does not.
//!
//! The ladder itself is deliberately tight — floating menus carry a lot of
//! information in a narrow column, so the steps are half-pixel apart and the
//! section headings sit a full step below the row text.

use gpui::{Rems, rems};

/// The ladder's values in logical pixels at the default 16px root.
pub const MENU_ROW: f32 = 12.5;
pub const MENU_SUBLINE: f32 = 11.0;
pub const MENU_HEADING: f32 = 10.0;
pub const MENU_BODY: f32 = 13.0;

/// `size` in logical pixels expressed as rems against the default 16px root.
pub fn ui_rems(size: f32) -> Rems {
    rems(size / 16.0)
}

/// A row's primary label.
pub fn menu_row() -> Rems {
    ui_rems(MENU_ROW)
}

/// A row's muted second line.
pub fn menu_subline() -> Rems {
    ui_rems(MENU_SUBLINE)
}

/// An uppercase section heading.
pub fn menu_heading() -> Rems {
    ui_rems(MENU_HEADING)
}

/// Free-standing body copy inside a menu (empty states, status lines).
pub fn menu_body() -> Rems {
    ui_rems(MENU_BODY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_rems_scales_against_the_default_root() {
        let cases = [(16.0_f32, 1.0_f32), (8.0, 0.5), (MENU_ROW, MENU_ROW / 16.0)];
        for (size, expected) in cases {
            assert!(
                (ui_rems(size).0 - expected).abs() < f32::EPSILON,
                "ui_rems({size}) should be {expected}"
            );
        }
        assert!((menu_row().0 - rems(MENU_ROW / 16.0).0).abs() < f32::EPSILON);
    }

    #[test]
    fn ladder_steps_down_from_row_to_heading() {
        let ladder = [MENU_ROW, MENU_SUBLINE, MENU_HEADING];
        for pair in ladder.windows(2) {
            assert!(pair[0] > pair[1], "ladder must descend: {pair:?}");
        }
    }
}

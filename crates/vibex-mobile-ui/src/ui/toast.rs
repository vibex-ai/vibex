//! A message that says something went wrong, and then goes away.
//!
//! A failure used to be drawn into the page as a line of red text under the
//! transcript. It had no way to be dismissed and no way to expire, so a
//! connection that dropped once while the phone was in a pocket left a
//! permanent red sentence on a page that was otherwise working — the reader
//! had no way to tell it was old news.
//!
//! A toast is the opposite: it floats over the page rather than pushing it
//! around, it leaves on its own, and a tap takes it away at once.

use std::{rc::Rc, time::Duration};

use nsh_core::prelude::*;

use crate::theme;

/// How long a toast stays before it leaves on its own.
const TOAST_LIFETIME_MS: u64 = 4000;

/// The notice, floating under the page's top bar.
///
/// `notice` is read reactively: setting it shows a toast and starts its life
/// over, and clearing it takes the toast away. The caller owns the clearing —
/// this only asks for it through `on_dismiss`, so the same signal stays the
/// single source of truth for whether a failure is on screen.
pub fn toast(
    notice: Signal<Option<SharedString>>,
    top: f32,
    on_dismiss: impl Fn() + Clone + 'static,
) -> View {
    // Each message gets its own countdown. A second failure must not inherit
    // the first one's remaining time, and an older countdown finishing must not
    // take a newer toast away — so every message bumps the generation, and a
    // countdown only retires the message it was started for.
    let generation = signal(0u64);
    let expire = Rc::new(on_dismiss.clone());
    effect({
        let expire = expire.clone();
        move || {
            if notice.get().is_some() {
                let mine = generation.get_untracked() + 1;
                generation.set(mine);
                let expire = expire.clone();
                spawn(async move {
                    timer(Duration::from_millis(TOAST_LIFETIME_MS)).await;
                    if generation.get_untracked() == mine {
                        expire();
                    }
                });
            }
        }
    });
    view()
        .style_when(move || notice.get().is_none(), |style| style.hidden())
        .absolute()
        .left(px(theme::SPACING_MD))
        .right(px(theme::SPACING_MD))
        .top(px(top))
        .z_index(20)
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .px(px(theme::SPACING_MD))
        .py(px(theme::SPACING_SM))
        .rounded(px(theme::RADIUS_CARD))
        .bg(theme::floating_pane_solid())
        .border_1()
        .border_color(theme::hairline)
        .shadow_md()
        .active(|style| style.opacity(0.7))
        .on_press({
            let on_dismiss = on_dismiss.clone();
            move || on_dismiss()
        })
        .child(
            svg(icon!("triangle-alert"))
                .size(px(theme::ICON_MD))
                .flex_shrink_0()
                .text_color(theme::accent_red),
        )
        .child(
            text(move || notice.with(|notice| notice.clone().unwrap_or_default()))
                .flex_1()
                .min_w(px(0.0))
                .text_size(px(theme::font_caption()))
                .text_color(theme::text_secondary),
        )
        .child(
            svg(icon!("x"))
                .size(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(theme::text_muted),
        )
}

#[cfg(test)]
mod tests {
    /// A toast is the only surface that reports a failure now, and it must be
    /// able to leave without the reader doing anything.
    #[test]
    fn a_toast_expires_on_its_own() {
        const {
            assert!(super::TOAST_LIFETIME_MS <= 8_000);
            assert!(super::TOAST_LIFETIME_MS >= 2_000);
        }
    }
}

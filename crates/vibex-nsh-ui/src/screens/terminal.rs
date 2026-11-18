//! A remote terminal, shown full-screen over the session shell.
//!
//! The bytes come from the runtime's PTY (`TerminalBackend`), and what the
//! person types goes back to it. `nsh`'s `Terminal` draws the emulator's grid
//! with the platform's text engine and reports input, so this module only
//! wires the two directions and the close action.

use std::rc::Rc;

use nsh_base::{Terminal, TerminalEvent, terminal as terminal_view};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::{copy, theme, ui::action};

/// How the terminal screen reaches the session.
#[derive(Clone)]
pub struct TerminalHost {
    pub title: Signal<Option<SharedString>>,
    pub error: Signal<Option<SharedString>>,
    /// The reference the service writes the program's output through.
    pub term_ref: ComponentRef<Terminal>,
    /// What the person typed (and pasted), for the program.
    pub on_data: Rc<dyn Fn(&str)>,
    /// The grid's new size, in columns and rows.
    pub on_resize: Rc<dyn Fn(u32, u32)>,
    /// The title the program set.
    pub on_title: Rc<dyn Fn(&str)>,
    pub on_close: Rc<dyn Fn()>,
}

/// The terminal screen.
pub fn terminal(host: TerminalHost) -> impl IntoElement {
    let on_data = host.on_data.clone();
    let on_resize = host.on_resize.clone();
    let on_title = host.on_title.clone();
    let title = host.title;
    let error = host.error;
    view()
        .flex_1()
        .bg(theme::background)
        .flex_col()
        .child(
            view()
                .w_full()
                .pt_safe_area()
                .border_b_1()
                .border_b_color(theme::border_subtle)
                .child(
                    view()
                        .w_full()
                        .h(px(48.0))
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_XS))
                        .px(px(theme::SPACING_SM))
                        .child(action::icon_button(
                            icon(include_str!("../../assets/icons/chevron-left.svg")),
                            copy::text("Back", "返回", "返回"),
                            {
                                let on_close = host.on_close.clone();
                                move || on_close()
                            },
                        ))
                        .child(
                            text(move || {
                                title.get().unwrap_or_else(|| {
                                    copy::text("Terminal", "终端", "終端").into()
                                })
                            })
                            .flex_1()
                            .truncate()
                            .text_size(px(theme::FONT_HEADING))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                        ),
                ),
        )
        .child(when(
            move || error.get().is_some(),
            move || {
                text(move || error.get().unwrap_or_default())
                    .px(px(theme::SPACING_LG))
                    .py(px(theme::SPACING_SM))
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::accent_red)
            },
        ))
        .child(
            terminal_view()
                .component_ref(&host.term_ref)
                .accessibility_label(copy::text("Terminal", "终端", "終端"))
                .on_data(move |data: &SharedString| on_data(data))
                .on_resize(move |event: &TerminalEvent| {
                    if let TerminalEvent::Resize(cols, rows) = event {
                        on_resize(*cols, *rows);
                    }
                })
                .on_title_change(move |title: &SharedString| on_title(title))
                .flex_1(),
        )
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

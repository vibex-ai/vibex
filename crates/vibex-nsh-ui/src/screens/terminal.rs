//! A remote terminal, shown full-screen over the session shell.
//!
//! The bytes come from the runtime's PTY (`TerminalBackend`), and what the
//! person types goes back to it. `nsh`'s `Terminal` draws the emulator's grid
//! with the platform's text engine and reports input, so this module only
//! wires the two directions and the close action.

use std::rc::Rc;

use nsh_base::{SheetEvent, Terminal, TerminalEvent, button, sheet, terminal as terminal_view};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::{copy, theme, ui::action};

/// One of the workspace's terminals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalChoice {
    pub id: String,
    pub title: String,
    /// "running", "exited" or "stale".
    pub status: String,
}

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
    /// The workspace's terminals, and whether the list is open.
    pub terminals: Signal<Vec<TerminalChoice>>,
    pub terminal_menu: Signal<bool>,
    pub on_open_menu: Rc<dyn Fn()>,
    pub on_close_menu: Rc<dyn Fn()>,
    pub on_select_terminal: Rc<dyn Fn(&str)>,
    pub on_new_terminal: Rc<dyn Fn()>,
    pub on_close: Rc<dyn Fn()>,
}

/// The terminal screen.
pub fn terminal(host: TerminalHost) -> impl IntoElement {
    let on_data = host.on_data.clone();
    let on_resize = host.on_resize.clone();
    let on_title = host.on_title.clone();
    let terminal_menu = host.terminal_menu;
    let terminals = host.terminals;
    let title = host.title;
    let error = host.error;
    view()
        .flex_1()
        .bg(theme::background)
        .flex_col()
        .pb_safe_area()
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
                            .text_size(px(theme::font_heading()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                        )
                        .child({
                            let on_open_menu = host.on_open_menu.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/menu.svg")),
                                copy::text("Terminals", "终端列表", "終端清單"),
                                move || on_open_menu(),
                            )
                        }),
                ),
        )
        .child(when(
            move || error.get().is_some(),
            move || {
                text(move || error.get().unwrap_or_default())
                    .px(px(theme::SPACING_LG))
                    .py(px(theme::SPACING_SM))
                    .text_size(px(theme::font_caption()))
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
        .child(terminal_sheet(
            terminal_menu,
            terminals,
            host.on_close_menu.clone(),
            host.on_select_terminal.clone(),
            host.on_new_terminal.clone(),
        ))
}

/// The list of the workspace's terminals, with New terminal.
fn terminal_sheet(
    open: Signal<bool>,
    terminals: Signal<Vec<TerminalChoice>>,
    on_close: Rc<dyn Fn()>,
    on_select: Rc<dyn Fn(&str)>,
    on_new: Rc<dyn Fn()>,
) -> impl IntoElement {
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event
                && !next
            {
                on_close();
            }
        })
        .child(
            view()
                .w_full()
                .gap(px(theme::SPACING_SM))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(16.0))
                .child(
                    text(copy::text("Terminals", "终端", "終端"))
                        .text_size(px(theme::font_heading()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(each(
                    move || terminals.get(),
                    |terminal| terminal.id.clone(),
                    move |terminal, _| {
                        let on_select = on_select.clone();
                        button()
                            .w_full()
                            .min_h(px(48.0))
                            .px(px(theme::SPACING_MD))
                            .py(px(theme::SPACING_XS))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .bg(theme::background_dim)
                            .flex_col()
                            .items_start()
                            .active(|style| style.opacity(0.8))
                            .on_press(move || on_select(&terminal.get_untracked().id))
                            .child(
                                text(move || terminal.get().title)
                                    .truncate()
                                    .text_size(px(theme::font_detail()))
                                    .text_color(theme::foreground),
                            )
                            .child(
                                text(move || terminal.get().status)
                                    .text_size(px(theme::font_micro()))
                                    .text_color(theme::text_muted),
                            )
                    },
                ))
                .child({
                    let on_new = on_new.clone();
                    action::primary(
                        copy::text("New terminal", "新建终端", "新增終端"),
                        icon(include_str!("../../assets/icons/terminal.svg")),
                        move || on_new(),
                    )
                    .w_full()
                }),
        )
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

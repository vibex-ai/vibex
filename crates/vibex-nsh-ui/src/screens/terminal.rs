//! Workspace terminal tabs and the native terminal emulator. The authority owns the PTYs.

use std::rc::Rc;

use nsh_base::{Terminal, TerminalApi, TerminalEvent, button, terminal as terminal_view};
use nsh_core::prelude::*;

use crate::{
    copy, theme,
    ui::{action, field},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalChoice {
    pub id: String,
    pub title: String,
    pub status: String,
}

type TerminalAction = Rc<dyn Fn(&str)>;

#[derive(Clone)]
pub struct TerminalHost {
    pub session_terminal: Memo<Option<crate::screens::session::SessionSummary>>,
    pub title: Signal<Option<SharedString>>,
    pub error: Signal<Option<SharedString>>,
    pub term_ref: ComponentRef<Terminal>,
    pub on_data: TerminalAction,
    pub on_resize: Rc<dyn Fn(u32, u32)>,
    pub on_title: TerminalAction,
    pub terminals: Signal<Vec<TerminalChoice>>,
    pub active: Signal<Option<String>>,
    pub busy: Signal<bool>,
    pub output_revision: Signal<u64>,
    pub on_retry: Rc<dyn Fn()>,
    pub on_kill: TerminalAction,
    pub on_select_terminal: TerminalAction,
    pub on_new_terminal: Rc<dyn Fn()>,
}

pub fn terminal(host: TerminalHost) -> impl IntoElement {
    let active = host.active;
    let busy = host.busy;
    let term_ref = host.term_ref;
    let find_open = signal(false);
    let find_query = signal(String::new());
    let control = signal(false);
    let alt = signal(false);
    let confirm = signal(None::<String>);
    let error = host.error;
    let output = host.output_revision;
    let selected = move || active.with(Option::is_some);
    let attached_session = move || {
        host.session_terminal
            .with(|session| session.as_ref().is_some_and(|session| session.terminal))
    };
    let on_data: TerminalAction = Rc::new({
        let send = host.on_data.clone();
        move |text| {
            if !text.is_empty() && selected() && !busy.get_untracked() {
                send(text);
            }
        }
    });
    effect(move || {
        active.get();
        control.set(false);
        alt.set(false);
        confirm.set(None);
    });
    let find_count = computed(move || {
        output.get();
        let query = find_query.with(|query| query.trim().to_lowercase());
        if query.is_empty() {
            return 0;
        }
        term_ref.api::<TerminalApi>().map_or(0, |api| {
            api.text().to_lowercase().matches(query.as_str()).count()
        })
    });
    view()
        .flex_1()
        .min_h(px(0.0))
        .bg(theme::background)
        .pb_keyboard()
        .child(when(
            move || find_open.get(),
            move || {
                view()
                    .h(px(theme::TOUCH_TARGET))
                    .flex_shrink_0()
                    .px(px(theme::SPACING_XS))
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_XS))
                    .border_b_1()
                    .border_b_color(theme::border_subtle)
                    .child(
                        field::text_field_flat(
                            find_query,
                            copy::text("Find in terminal", "在终端中查找", "在終端中尋找"),
                        )
                        .flex_1()
                        .min_w(px(0.0)),
                    )
                    .child(
                        text(move || {
                            format!(
                                "{} {}",
                                find_count.get(),
                                copy::text("matches", "个匹配", "個匹配")
                            )
                        })
                        .flex_shrink_0()
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_muted),
                    )
                    .child(
                        action::icon_button(
                            icon!("x"),
                            copy::text("Close search", "关闭搜索", "關閉搜尋"),
                            move || {
                                find_open.set(false);
                                find_query.set(String::new());
                                dismiss_keyboard();
                            },
                        )
                        .flex_shrink_0(),
                    )
            },
        ))
        .child(
            view()
                .h(px(theme::TOUCH_TARGET))
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .pl(px(theme::SPACING_SM))
                .pr(px(theme::SPACING_XS))
                .border_b_1()
                .border_b_color(theme::border_subtle)
                .child(
                    when(attached_session, move || {
                        text(move || host.title.get().unwrap_or_default())
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::foreground)
                    })
                    .otherwise({
                        let select = host.on_select_terminal.clone();
                        let terminals = host.terminals;
                        move || terminal_tabs(terminals, active, busy, confirm, select.clone())
                    }),
                )
                .child(
                    action::icon_button(
                        icon!("search"),
                        copy::text("Find in terminal", "在终端中查找", "在終端中尋找"),
                        move || {
                            find_open.update(|open| *open = !*open);
                            if !find_open.get_untracked() {
                                dismiss_keyboard();
                            }
                        },
                    )
                    .size(px(theme::HEADER_TRAILING_BUTTON_SIZE))
                    .flex_shrink_0()
                    .disabled(move || !selected())
                    .disabled_style(|style| style.opacity(0.4)),
                )
                .child(when(move || !attached_session(), {
                    let create = host.on_new_terminal.clone();
                    move || {
                        let create = create.clone();
                        action::icon_button(
                            icon!("plus"),
                            copy::text("New terminal", "新建终端", "新增終端"),
                            move || create(),
                        )
                        .size(px(theme::HEADER_TRAILING_BUTTON_SIZE))
                        .flex_shrink_0()
                        .disabled(move || busy.get())
                        .disabled_style(|style| style.opacity(0.4))
                    }
                })),
        )
        .child(when(move || confirm.with(Option::is_some), {
            let kill = host.on_kill.clone();
            move || {
                let kill = kill.clone();
                view()
                    .m(px(theme::SPACING_SM))
                    .p(px(theme::SPACING_SM))
                    .gap(px(theme::SPACING_SM))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .bg(theme::background_card)
                    .child(
                        text(copy::text(
                            "Close this terminal and stop its running process?",
                            "关闭此终端并停止正在运行的进程？",
                            "關閉此終端機並停止正在執行的程序？",
                        ))
                        .text_size(px(theme::font_caption()))
                        .text_color(theme::text_secondary),
                    )
                    .child(
                        view()
                            .flex_row()
                            .justify_end()
                            .gap(px(theme::SPACING_SM))
                            .child(action::secondary(
                                copy::text("Cancel", "取消", "取消"),
                                icon!("x"),
                                false,
                                move || confirm.set(None),
                            ))
                            .child(action::destructive(
                                copy::text("Close terminal", "关闭终端", "關閉終端機"),
                                icon!("x"),
                                move || {
                                    if let Some(id) = confirm.get_untracked() {
                                        confirm.set(None);
                                        kill(&id);
                                    }
                                },
                            )),
                    )
            }
        }))
        .child(when(move || error.with(Option::is_some), {
            let retry = host.on_retry.clone();
            move || {
                let retry = retry.clone();
                view()
                    .flex_row()
                    .items_center()
                    .px(px(theme::SPACING_SM))
                    .gap(px(theme::SPACING_XS))
                    .child(
                        text(move || error.get().unwrap_or_default())
                            .flex_1()
                            .min_w(px(0.0))
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::accent_red),
                    )
                    .child(
                        action::icon_button(
                            icon!("refresh"),
                            copy::text("Reconnect terminal", "重连终端", "重新連接終端機"),
                            move || retry(),
                        )
                        .flex_shrink_0(),
                    )
            }
        }))
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .w_full()
                .relative()
                .child({
                    let data = on_data.clone();
                    terminal_view()
                        .component_ref(&term_ref)
                        .accessibility_label(copy::text("Terminal input", "终端输入", "終端機輸入"))
                        .on_data(move |text: &SharedString| {
                            if text
                                .chars()
                                .next()
                                .is_some_and(|character| !character.is_control())
                            {
                                let text = modified_input(
                                    text,
                                    control.get_untracked(),
                                    alt.get_untracked(),
                                );
                                control.set(false);
                                alt.set(false);
                                data(&text);
                            } else {
                                data(text);
                            }
                        })
                        .on_resize(move |event: &TerminalEvent| {
                            if let TerminalEvent::Resize(cols, rows) = event {
                                (host.on_resize)(*cols, *rows);
                            }
                        })
                        .on_title_change(move |title: &SharedString| (host.on_title)(title))
                        .w_full()
                        .flex_1()
                        .min_h(px(0.0))
                })
                .child(when(
                    move || !selected(),
                    move || {
                        view()
                            .absolute()
                            .inset(px(0.0))
                            .bg(theme::background)
                            .items_center()
                            .justify_center()
                            .child(
                                text(move || {
                                    if busy.get() {
                                        copy::text(
                                            "Connecting to terminal…",
                                            "正在连接终端…",
                                            "正在連接終端機…",
                                        )
                                    } else {
                                        copy::text(
                                            "Select or create a terminal",
                                            "选择或新建终端",
                                            "選擇或新增終端機",
                                        )
                                    }
                                })
                                .text_size(px(theme::font_caption()))
                                .text_color(theme::text_muted),
                            )
                    },
                )),
        )
        .child(when(selected, move || {
            terminal_keys(term_ref, control, alt, on_data.clone())
        }))
        .child(when(
            move || keyboard_height().0 == 0.0,
            || view().h(safe_area().bottom),
        ))
}

fn terminal_tabs(
    terminals: Signal<Vec<TerminalChoice>>,
    active: Signal<Option<String>>,
    busy: Signal<bool>,
    confirm: Signal<Option<String>>,
    select: TerminalAction,
) -> impl IntoElement {
    scroll()
        .horizontal()
        .scrollbar(false)
        .flex_1()
        .min_w(px(0.0))
        .child(
            view()
                .h(px(theme::TOUCH_TARGET))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
                .child(each(
                    move || {
                        terminals.with(|choices| {
                            choices
                                .iter()
                                .filter(|choice| choice.status == "running")
                                .cloned()
                                .collect::<Vec<_>>()
                        })
                    },
                    |choice| choice.id.clone(),
                    move |choice, _| {
                        let select = select.clone();
                        let id = choice.get_untracked().id;
                        view()
                            .h(px(30.0))
                            .flex_shrink_0()
                            .flex_row()
                            .items_center()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .style_when(
                                move || active.with(|active| active.as_deref() == Some(&id)),
                                |style| style.bg(theme::rail_tab_bg()),
                            )
                            .child(
                                button()
                                    .h_full()
                                    .max_w(px(132.0))
                                    .px(px(theme::SPACING_SM))
                                    .justify_center()
                                    .on_press(move || {
                                        dismiss_keyboard();
                                        select(&choice.get_untracked().id);
                                    })
                                    .child(
                                        text(move || choice.with(|choice| choice.title.clone()))
                                            .truncate()
                                            .text_size(px(theme::font_caption()))
                                            .text_color(theme::foreground),
                                    ),
                            )
                            .child(
                                button()
                                    .size(px(28.0))
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .accessibility_label(move || {
                                        Some(
                                            format!(
                                                "{}: {}",
                                                copy::text(
                                                    "Close terminal",
                                                    "关闭终端",
                                                    "關閉終端機"
                                                ),
                                                choice.with(|choice| choice.title.clone())
                                            )
                                            .into(),
                                        )
                                    })
                                    .on_press(move || {
                                        dismiss_keyboard();
                                        confirm.set(Some(choice.get_untracked().id));
                                    })
                                    .disabled(move || busy.get())
                                    .disabled_style(|style| style.opacity(0.4))
                                    .child(
                                        svg(icon!("x"))
                                            .size(px(12.0))
                                            .text_color(theme::text_muted),
                                    ),
                            )
                    },
                )),
        )
}

fn terminal_keys(
    term_ref: ComponentRef<Terminal>,
    control: Signal<bool>,
    alt: Signal<bool>,
    data: TerminalAction,
) -> impl IntoElement {
    let keys = [
        ("Tab", "\t"),
        ("↑", "\x1b[A"),
        ("↓", "\x1b[B"),
        ("←", "\x1b[D"),
        ("→", "\x1b[C"),
        ("Enter", "\r"),
        ("⌫", "\u{7f}"),
    ];
    let escape = data.clone();
    let paste = data.clone();
    scroll()
        .horizontal()
        .scrollbar(false)
        .flex_shrink_0()
        .w_full()
        .border_t_1()
        .border_t_color(theme::border_subtle)
        .child(
            view()
                .h(px(52.0))
                .flex_row()
                .items_center()
                .px(px(theme::SPACING_SM))
                .gap(px(theme::SPACING_XS))
                .child(key_chip("Esc", || false, move || escape("\x1b")))
                .child(key_chip(
                    "Ctrl",
                    move || control.get(),
                    move || control.update(|selected| *selected = !*selected),
                ))
                .children(keys.map(|(label, sequence)| {
                    let data = data.clone();
                    key_chip(label, || false, move || data(sequence))
                }))
                .child(key_chip(
                    "Alt",
                    move || alt.get(),
                    move || alt.update(|selected| *selected = !*selected),
                ))
                .child(key_chip(
                    copy::text("Paste", "粘贴", "貼上"),
                    || false,
                    move || {
                        let paste = paste.clone();
                        spawn(async move {
                            paste(&clipboard_string().await.replace('\n', "\r"));
                        });
                    },
                ))
                .child(key_chip(
                    copy::text("Keyboard", "键盘", "鍵盤"),
                    || false,
                    move || {
                        if let Some(api) = term_ref.api::<TerminalApi>() {
                            api.focus();
                        }
                    },
                )),
        )
}

fn key_chip(
    label: &'static str,
    selected: impl Fn() -> bool + 'static,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .h(px(32.0))
        .px(px(theme::SPACING_SM))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_CONTROL))
        .bg(theme::background_dim)
        .style_when(selected, |style| style.bg(theme::sidebar_selected_bg()))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .on_press(action)
        .child(
            text(label)
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_secondary),
        )
}

fn modified_input(text: &str, control: bool, alt: bool) -> String {
    let mut result = String::new();
    if alt {
        result.push('\x1b');
    }
    let mut characters = text.chars();
    if let Some(first) = characters.next() {
        let upper = first.to_ascii_uppercase();
        result.push(if control && ('@'..='_').contains(&upper) {
            char::from((upper as u8) & 0x1f)
        } else {
            first
        });
    }
    result.extend(characters);
    result
}

#[cfg(test)]
mod tests {
    use super::modified_input;

    #[test]
    fn latched_keys_apply_to_the_next_committed_character() {
        assert_eq!(modified_input("c", true, false), "\x03");
        assert_eq!(modified_input("x", false, true), "\x1bx");
        assert_eq!(modified_input("你好", true, false), "你好");
        assert_eq!(modified_input("abc", true, true), "\x1b\x01bc");
    }
}

//! Workspace terminal tabs and the native terminal emulator. The authority owns the PTYs.

use std::{cell::RefCell, rc::Rc};

use nsh_base::{Terminal, TerminalApi, TerminalEvent, button, input, terminal as terminal_view};
use nsh_core::prelude::*;
use nsh_core::{AutoCapitalize, AutoFill, SubmitBehavior, element::Composition};

use crate::{
    copy, theme,
    ui::{action, field, surface},
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
    pub find_open: Signal<bool>,
    pub on_retry: Rc<dyn Fn()>,
    pub on_kill: TerminalAction,
    pub on_select_terminal: TerminalAction,
    pub on_new_terminal: Rc<dyn Fn()>,
}

pub fn terminal(host: TerminalHost) -> impl IntoElement {
    let active = host.active;
    let busy = host.busy;
    let term_ref = host.term_ref;
    let find_open = host.find_open;
    let find_query = signal(String::new());
    let control = signal(false);
    let alt = signal(false);
    let keyboard = element_ref();
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
    let on_text: TerminalAction = Rc::new({
        let on_data = on_data.clone();
        move |text: &str| {
            if text.starts_with(|character: char| !character.is_control()) {
                let text = modified_input(text, control.get_untracked(), alt.get_untracked());
                control.set(false);
                alt.set(false);
                on_data(&text);
            } else {
                on_data(text);
            }
        }
    });
    effect(move || {
        active.get();
        control.set(false);
        alt.set(false);
        confirm.set(None);
        find_open.set(false);
    });
    effect(move || {
        if !find_open.get() {
            find_query.set(String::new());
        }
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
        .pb_keyboard()
        // The head: the terminals of the workspace as tabs, or the one a
        // terminal session runs in, and the tools that act on the open one.
        .child(
            view()
                .h(px(theme::TOUCH_TARGET))
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .pl(px(theme::SPACING_MD))
                .pr(px(theme::SPACING_SM))
                .child(
                    when(attached_session, move || {
                        view()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex_row()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .child(surface::field_glyph(icon!("square-terminal")))
                            .child(
                                text(move || host.title.get().unwrap_or_default())
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_size(px(theme::font_detail()))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::foreground),
                            )
                    })
                    .otherwise({
                        let select = host.on_select_terminal.clone();
                        let terminals = host.terminals;
                        move || terminal_tabs(terminals, active, busy, confirm, select.clone())
                    }),
                )
                .child(
                    surface::ghost_button(
                        icon!("keyboard"),
                        copy::text("Keyboard", "键盘", "鍵盤"),
                        move || keyboard.focus(),
                    )
                    .disabled(move || !selected() || busy.get()),
                )
                .child(
                    surface::ghost_button(
                        icon!("search"),
                        copy::text("Find in terminal", "在终端中查找", "在終端中尋找"),
                        move || {
                            find_open.update(|open| *open = !*open);
                            if !find_open.get_untracked() {
                                dismiss_keyboard();
                            }
                        },
                    )
                    .disabled(move || !selected()),
                )
                .child(when(move || !attached_session(), {
                    let create = host.on_new_terminal.clone();
                    move || {
                        let create = create.clone();
                        surface::ghost_button(
                            icon!("plus"),
                            copy::text("New terminal", "新建终端", "新增終端"),
                            move || create(),
                        )
                        .disabled(move || busy.get())
                    }
                })),
        )
        .child(when(
            move || find_open.get(),
            move || {
                view()
                    .flex_shrink_0()
                    .flex_row()
                    .items_center()
                    .gap(px(2.0))
                    .pl(px(theme::SPACING_MD))
                    .pr(px(theme::SPACING_SM))
                    .pt(px(theme::SPACING_XS))
                    .child(
                        surface::field_pill()
                            .flex_1()
                            .min_w(px(0.0))
                            .pr(px(theme::SPACING_MD))
                            .mr(px(theme::SPACING_XS))
                            .child(surface::field_glyph(icon!("search")))
                            .child(
                                field::text_field_bare(
                                    find_query,
                                    copy::text("Find in terminal", "在终端中查找", "在終端中尋找"),
                                )
                                .auto_focus(true)
                                .flex_1()
                                .min_w(px(0.0))
                                .h_full(),
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
                            ),
                    )
                    .child(surface::ghost_button(
                        icon!("x"),
                        copy::text("Close search", "关闭搜索", "關閉搜尋"),
                        move || {
                            find_open.set(false);
                            dismiss_keyboard();
                        },
                    ))
            },
        ))
        .child(when(move || confirm.with(Option::is_some), {
            let kill = host.on_kill.clone();
            move || {
                let kill = kill.clone();
                // Closing a terminal ends what runs in it, so it is asked for
                // twice: once on the tab, once here.
                view()
                    .flex_shrink_0()
                    .px(px(theme::SPACING_MD))
                    .pt(px(theme::SPACING_SM))
                    .child(
                        surface::card()
                            .p(px(surface::ROW_INSET))
                            .gap(px(theme::SPACING_MD))
                            .child(
                                text(copy::text(
                                    "Close this terminal and stop its running process?",
                                    "关闭此终端并停止正在运行的进程？",
                                    "關閉此終端機並停止正在執行的程序？",
                                ))
                                .text_size(px(theme::font_detail()))
                                .line_height(theme::line_height_for(theme::font_detail()))
                                .text_color(theme::foreground),
                            )
                            .child(
                                view()
                                    .flex_row()
                                    .gap(px(theme::SPACING_SM))
                                    .child(
                                        action::tonal(
                                            copy::text("Cancel", "取消", "取消"),
                                            move || confirm.set(None),
                                        )
                                        .flex_1()
                                        .min_w(px(0.0)),
                                    )
                                    .child(
                                        action::tonal_danger(
                                            copy::text("Close terminal", "关闭终端", "關閉終端機"),
                                            move || {
                                                if let Some(id) = confirm.get_untracked() {
                                                    confirm.set(None);
                                                    kill(&id);
                                                }
                                            },
                                        )
                                        .flex_1()
                                        .min_w(px(0.0)),
                                    ),
                            ),
                    )
            }
        }))
        .child(when(move || error.with(Option::is_some), {
            let retry = host.on_retry.clone();
            move || {
                let retry = retry.clone();
                view()
                    .flex_shrink_0()
                    .mx(px(theme::SPACING_MD))
                    .mt(px(theme::SPACING_SM))
                    .pl(px(theme::SPACING_MD))
                    .pr(px(theme::SPACING_XS))
                    .min_h(px(theme::ROUND_CONTROL))
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    .rounded(px(theme::RADIUS_CHIP))
                    .bg(move || theme::tint(theme::accent_red()))
                    .child(
                        text(move || error.get().unwrap_or_default())
                            .flex_1()
                            .min_w(px(0.0))
                            .py(px(theme::SPACING_SM))
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::accent_red),
                    )
                    .child(surface::ghost_button(
                        icon!("refresh"),
                        copy::text("Reconnect terminal", "重连终端", "重新連接終端機"),
                        move || retry(),
                    ))
            }
        }))
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .w_full()
                .mt(px(theme::SPACING_XS))
                .relative()
                .child({
                    let data = on_text.clone();
                    terminal_view()
                        .component_ref(&term_ref)
                        .accessibility_label(copy::text("Terminal grid", "终端网格", "終端機網格"))
                        .on_data(move |text: &SharedString| data(text))
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
                .child(terminal_keyboard(keyboard, active, on_text))
                .child(when(
                    move || !selected(),
                    move || {
                        view()
                            .absolute()
                            .inset(px(0.0))
                            .bg(theme::block_page_adaptive())
                            .child(surface::empty_state(
                                icon!("square-terminal"),
                                move || {
                                    SharedString::from(if busy.get() {
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
                                    })
                                },
                                copy::text(
                                    "A terminal runs in the session's workspace on the desktop.",
                                    "终端运行在桌面端该会话的工作区中。",
                                    "終端機在桌面端該工作階段的工作區中執行。",
                                ),
                            ))
                    },
                )),
        )
        .child(when(selected, move || {
            terminal_keys(control, alt, on_data.clone())
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
                .gap(px(6.0))
                .pr(px(theme::SPACING_SM))
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
                        let chosen =
                            move || active.with(|active| active.as_deref() == Some(id.as_str()));
                        // A tab: the terminal's name, and the way to close it
                        // inside the same pill.
                        view()
                            .h(px(theme::CHIP_HEIGHT))
                            .flex_shrink_0()
                            .flex_row()
                            .items_center()
                            .rounded_full()
                            .bg({
                                let chosen = chosen.clone();
                                move || {
                                    if chosen() {
                                        theme::fill_strong()
                                    } else {
                                        theme::fill()
                                    }
                                }
                            })
                            .child(
                                button()
                                    .h_full()
                                    .max_w(px(132.0))
                                    .pl(px(theme::SPACING_MD))
                                    .pr(px(theme::SPACING_XS))
                                    .justify_center()
                                    .on_press(move || {
                                        dismiss_keyboard();
                                        select(&choice.get_untracked().id);
                                    })
                                    .child(
                                        text(move || choice.with(|choice| choice.title.clone()))
                                            .truncate()
                                            .text_size(px(theme::font_micro()))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(move || {
                                                if chosen() {
                                                    theme::foreground()
                                                } else {
                                                    theme::text_secondary()
                                                }
                                            }),
                                    ),
                            )
                            .child(
                                button()
                                    .size(px(28.0))
                                    .mr(px(2.0))
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
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
        .child(
            view()
                .h(px(48.0))
                .flex_row()
                .items_center()
                .px(px(theme::SPACING_MD))
                .gap(px(6.0))
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
                            paste(&terminal_newlines(&clipboard_string().await));
                        });
                    },
                )),
        )
}

const INPUT_SEED: &str = " ";

fn terminal_keyboard(
    keyboard: ElementRef,
    active: Signal<Option<String>>,
    send: TerminalAction,
) -> impl IntoElement {
    let value = signal(INPUT_SEED.to_owned());
    let pending = Rc::new(RefCell::new(None::<String>));
    let composing = signal(false);
    let reset = {
        let pending = pending.clone();
        move || {
            composing.set(false);
            pending.borrow_mut().take();
            value.set(INPUT_SEED.to_owned());
        }
    };
    effect({
        let reset = reset.clone();
        move || {
            active.get();
            keyboard.blur();
            reset();
        }
    });
    let commit = {
        let send = send.clone();
        move |next: &str| {
            let data = value.with_untracked(|previous| terminal_edit(previous, next));
            value.set(if next.is_empty() {
                INPUT_SEED.to_owned()
            } else {
                next.to_owned()
            });
            if !data.is_empty() {
                send(&data);
            }
        }
    };
    input()
        .element_ref(&keyboard)
        .absolute()
        .top_0()
        .left_0()
        .size(px(theme::TOUCH_TARGET))
        .opacity(0.02)
        .text_color(theme::foreground().opacity(0.0))
        .caret(false)
        .pointer_events(PointerEvents::None)
        .label(copy::text("Terminal input", "终端输入", "終端機輸入"))
        .auto_correct(false)
        .auto_capitalize(AutoCapitalize::None)
        .auto_fill(AutoFill::Off)
        .submit_behavior(SubmitBehavior::Submit)
        .value(move || value.get())
        .selected_range(move || {
            let end = value.with(|value| value.encode_utf16().count() as u32);
            (end, end)
        })
        .on_change({
            let pending = pending.clone();
            let commit = commit.clone();
            move |next: &str| {
                if composing.get_untracked() {
                    pending.replace(Some(next.to_owned()));
                } else {
                    commit(next);
                }
            }
        })
        .on_composition_change(move |composition: &Composition| {
            composing.set(composition.composing);
            if !composition.composing
                && let Some(next) = pending.take()
            {
                commit(&next);
            }
        })
        .on_submit({
            let reset = reset.clone();
            move |_: &str| {
                send("\r");
                reset();
            }
        })
        .on_blur(reset)
}

fn terminal_edit(previous: &str, next: &str) -> String {
    let common = previous
        .chars()
        .zip(next.chars())
        .take_while(|(before, after)| before == after)
        .map(|(character, _)| character.len_utf8())
        .sum::<usize>();
    let mut bytes = "\u{7f}".repeat(previous[common..].chars().count());
    bytes.push_str(&terminal_newlines(&next[common..]));
    bytes
}

fn terminal_newlines(text: &str) -> String {
    text.replace("\r\n", "\r").replace('\n', "\r")
}

/// One key of the row the software keyboard lacks. A latched modifier is
/// filled with the ink until the next character takes it.
fn key_chip(
    label: &'static str,
    selected: impl Fn() -> bool + Copy + 'static,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .h(px(theme::COMPACT_CONTROL))
        .min_w(px(40.0))
        .px(px(10.0))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_TILE))
        .bg(move || {
            if selected() {
                theme::action_primary()
            } else {
                theme::fill()
            }
        })
        .active(|style| style.opacity(0.6))
        .on_press(action)
        .child(
            text(label)
                .font_family("monospace")
                .text_size(px(theme::font_micro()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || {
                    if selected() {
                        theme::action_primary_label()
                    } else {
                        theme::text_secondary()
                    }
                }),
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
    use super::{modified_input, terminal_edit};

    #[test]
    fn native_input_edits_produce_pty_bytes() {
        assert_eq!(terminal_edit(" ", " 你好"), "你好");
        assert_eq!(terminal_edit(" 你好", " 你"), "\u{7f}");
        assert_eq!(terminal_edit(" ", ""), "\u{7f}");
        assert_eq!(terminal_edit(" 你", " 您"), "\u{7f}您");
        assert_eq!(terminal_edit(" ", " ls\r\npwd\n"), "ls\rpwd\r");
    }

    #[test]
    fn latched_keys_apply_to_the_next_committed_character() {
        assert_eq!(modified_input("c", true, false), "\x03");
        assert_eq!(modified_input("x", false, true), "\x1bx");
        assert_eq!(modified_input("你好", true, false), "你好");
        assert_eq!(modified_input("abc", true, true), "\x1b\x01bc");
    }
}

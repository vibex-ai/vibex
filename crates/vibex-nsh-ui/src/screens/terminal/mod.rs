//! Workspace terminal tabs and the native mobile terminal emulator. The
//! authority owns the PTYs.

mod emulator;
mod grid;

use std::{cell::RefCell, rc::Rc};

use nsh_base::{button, input};
use nsh_core::{
    AutoCapitalize, AutoFill, PasteBehavior, SubmitBehavior,
    element::{Composition, PasteEvent, StyleRefinement},
    keymap::When,
    prelude::*,
};

pub use self::grid::{Terminal, TerminalApi, TerminalEvent, notify_terminal_fonts_ready};
use self::emulator::EmulatorModes;
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
type SubmitHandle = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// The air between the grid's cells and the panel's edge.
const TERMINAL_INSET: f32 = 10.0;

/// The terminal panel, for the tests that check it is drawn as one.
const TERMINAL_PANEL_ID: &str = "terminal-panel";

/// The height of the row of keys the software keyboard lacks.
const TERMINAL_KEY_ROW_HEIGHT: f32 = 50.0;

/// One key's height in that row.
const TERMINAL_KEY_HEIGHT: f32 = 32.0;

/// Three space runways kept inside the invisible IME field so rapid
/// soft-keyboard Backspace presses always have characters to delete even before
/// a round-trip value sync arrives from Rust.
///
/// Using three distinct lengths guarantees every re-seed can pick a seed that:
/// 1. differs from the last `Prop::Value` flushed by `nsh-core` (so same-tick
///    `on_change` + `on_submit` never elides the reset),
/// 2. differs from the text currently in the native field (so `Core` always
///    emits `MountItem::InputValue`), and
/// 3. has a different leading-space prefix length than the pre-reseed text (so
///    we can unambiguously distinguish a dropped async `setValue` from fresh
///    input typed after the reset succeeded).
const INPUT_SEEDS: [&str; 3] = ["        ", "         ", "          "];

/// Minimum buffer length before the hidden IME field is re-seeded.
const INPUT_RESEED_LOW: usize = 4;

/// Maximum buffer length before the hidden IME field is re-seeded.
const INPUT_RESEED_HIGH: usize = 64;

/// The caret a terminal draws at the cursor's cell.
///
/// A block, not a bar: that is what a terminal's cursor looks like, and on a
/// phone it is the only mark that says where the next character lands. It takes
/// the terminal's accent so it reads against both the ground and a program's
/// own background colors.
fn cursor_style(style: StyleRefinement) -> StyleRefinement {
    style.bg(theme::accent_green().alpha(0.75)).rounded(px(1.0))
}

#[derive(Clone)]
pub struct TerminalHost {
    pub session_terminal: Memo<Option<crate::screens::session::SessionSummary>>,
    pub title: Signal<Option<SharedString>>,
    pub error: Signal<Option<SharedString>>,
    pub term_ref: ComponentRef<Terminal>,
    pub visible: Signal<bool>,
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
    let visible = host.visible;
    let term_ref = host.term_ref;
    let find_open = host.find_open;
    let find_query = signal(String::new());
    let composing_text = signal(None::<SharedString>);
    let control = signal(false);
    let alt = signal(false);
    let keyboard = element_ref();
    let submit_keyboard: SubmitHandle = Rc::new(RefCell::new(None));
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
    let focus_keyboard = Rc::new(move || {
        if visible.get_untracked() && selected() && !busy.get_untracked() {
            keyboard.focus();
        }
    });
    let terminal_modes = Rc::new(move || {
        term_ref
            .api::<TerminalApi>()
            .map(|api| api.modes())
            .unwrap_or_default()
    });
    effect(move || {
        active.get();
        control.set(false);
        alt.set(false);
        confirm.set(None);
        find_open.set(false);
        composing_text.set(None);
    });
    effect(move || {
        if !visible.get() {
            control.set(false);
            alt.set(false);
            confirm.set(None);
            composing_text.set(None);
        }
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
    let active_find_query = computed(move || {
        if !find_open.get() {
            return None;
        }
        let query = find_query.get();
        if query.trim().is_empty() {
            None
        } else {
            Some(SharedString::from(query))
        }
    });

    view()
        .flex_1()
        .min_h(px(0.0))
        .pb_keyboard()
        // The head: a terminal's own chrome rather than a page header. The tab
        // strip is the one thing that says "several shells, one of them open",
        // and it is drawn on the chrome tone so the grid below reads as the
        // recessed thing it is.
        .child(
            view()
                .h(px(theme::TOUCH_TARGET))
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .pl(px(theme::SPACING_MD))
                .pr(px(theme::SPACING_SM))
                .bg(theme::terminal_chrome())
                .border_b_1()
                .border_color(theme::terminal_hairline())
                .child(
                    when(attached_session, move || {
                        view()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex_row()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .child(
                                svg(icon!("square-terminal"))
                                    .size(px(theme::ICON_SM))
                                    .flex_shrink_0()
                                    .text_color(theme::terminal_key_label()),
                            )
                            .child(
                                text(move || host.title.get().unwrap_or_default())
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .font_family(theme::TERMINAL_FONT_FAMILY)
                                    .text_size(px(theme::font_caption()))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::terminal_key_label()),
                            )
                    })
                    .otherwise({
                        let select = host.on_select_terminal.clone();
                        let terminals = host.terminals;
                        move || terminal_tabs(terminals, active, busy, confirm, select.clone())
                    }),
                )
                .child({
                    let focus = focus_keyboard.clone();
                    surface::ghost_button(
                        icon!("keyboard"),
                        copy::text("Keyboard", "键盘", "鍵盤"),
                        move || focus(),
                    )
                    .disabled(move || !selected() || busy.get())
                })
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
                    .py(px(theme::SPACING_SM))
                    // The find bar belongs to the terminal's chrome: it opens
                    // under the tab strip, not on the page.
                    .bg(theme::terminal_chrome())
                    .border_b_1()
                    .border_color(theme::terminal_hairline())
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
        .child({
            let panel_focus = focus_keyboard.clone();
            let grid_focus = focus_keyboard.clone();
            let pty_reply = on_data.clone();
            let input_modes = terminal_modes.clone();
            view()
                .flex_1()
                .min_h(px(0.0))
                .w_full()
                .relative()
                // The grid's own ground and inset. A terminal is a recessed
                // panel, not a page: it keeps a little air around the cells so
                // the first column does not sit against the edge of the phone,
                // and it fills the whole width so a long line has room before
                // it wraps.
                .bg(theme::terminal_surface())
                .test_id(TERMINAL_PANEL_ID)
                .px(px(TERMINAL_INSET))
                .py(px(TERMINAL_INSET))
                .on_press(move || panel_focus())
                .child(
                    grid::terminal()
                        .component_ref(&term_ref)
                        .accessibility_label(copy::text("Terminal grid", "终端网格", "終端機網格"))
                        .font_family(theme::TERMINAL_FONT_FAMILY)
                        .font_size(px(theme::font_terminal()))
                        .line_height(px(theme::terminal_line_height()))
                        .cursor_style(cursor_style)
                        .composing(move || composing_text.get())
                        .find_query(move || active_find_query.get())
                        .on_data(move |text: &SharedString| pty_reply(text))
                        .on_resize(move |event: &TerminalEvent| {
                            if let TerminalEvent::Resize(cols, rows) = event {
                                (host.on_resize)(*cols, *rows);
                            }
                        })
                        .on_title_change(move |title: &SharedString| (host.on_title)(title))
                        .on_tap(move || grid_focus())
                        .w_full()
                        .flex_1()
                        .min_h(px(0.0)),
                )
                // Mount the single hidden IME input only while this terminal is
                // actively visible and attached. Leaving the tab or page cleanly
                // unmounts it (releasing Android's InputConnection and hiding the
                // soft keyboard), and returning mounts a clean InputBox with
                // fresh native geometry and edit count.
                .child(when(
                    move || visible.get() && selected(),
                    {
                        let submit_keyboard = submit_keyboard.clone();
                        move || {
                            terminal_keyboard(
                                keyboard,
                                active,
                                composing_text,
                                submit_keyboard.clone(),
                                input_modes.clone(),
                                on_text.clone(),
                            )
                        }
                    },
                ))
                .child(when(
                    move || !selected(),
                    move || {
                        view()
                            .absolute()
                            .inset(px(0.0))
                            .bg(theme::terminal_surface())
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
                ))
        })
        .child(when(selected, move || {
            terminal_keys(
                control,
                alt,
                terminal_modes.clone(),
                submit_keyboard.clone(),
                on_data.clone(),
            )
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
        .keyboard_taps(KeyboardTaps::Always)
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
                        view()
                            .h(px(theme::CHIP_HEIGHT))
                            .flex_shrink_0()
                            .flex_row()
                            .items_center()
                            .rounded(px(7.0))
                            .bg({
                                let chosen = chosen.clone();
                                move || {
                                    if chosen() {
                                        theme::terminal_tab_active()
                                    } else {
                                        theme::terminal_chrome()
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
                                            .font_family(theme::TERMINAL_FONT_FAMILY)
                                            .text_size(px(theme::font_micro()))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(move || {
                                                if chosen() {
                                                    theme::terminal_key_label()
                                                } else {
                                                    theme::terminal_key_label().alpha(0.55)
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
                                            .text_color(theme::terminal_key_label().alpha(0.6)),
                                    ),
                            )
                    },
                )),
        )
}

fn terminal_keys(
    control: Signal<bool>,
    alt: Signal<bool>,
    modes: Rc<dyn Fn() -> EmulatorModes>,
    submit_keyboard: SubmitHandle,
    data: TerminalAction,
) -> impl IntoElement {
    let escape = data.clone();
    let tab = data.clone();
    let enter = data.clone();
    let backspace = data.clone();
    let paste = data.clone();
    let paste_modes = modes.clone();
    let arrows: [(&'static str, char); 4] = [("↑", 'A'), ("↓", 'B'), ("←", 'D'), ("→", 'C')];

    scroll()
        .horizontal()
        .scrollbar(false)
        // Keep the software keyboard open when tapping Esc/Ctrl/Tab/arrows.
        .keyboard_taps(KeyboardTaps::Always)
        .flex_shrink_0()
        .w_full()
        .bg(theme::terminal_chrome())
        .border_t_1()
        .border_color(theme::terminal_hairline())
        .child(
            view()
                .h(px(TERMINAL_KEY_ROW_HEIGHT))
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
                .child(key_chip("Tab", || false, move || tab("\t")))
                .children(arrows.map(|(label, code)| {
                    let data = data.clone();
                    let modes = modes.clone();
                    key_chip(label, || false, move || {
                        let seq = if modes().application_cursor {
                            format!("\x1bO{code}")
                        } else {
                            format!("\x1b[{code}")
                        };
                        data(&seq);
                    })
                }))
                .child(key_chip("Enter", || false, move || {
                    let submit = submit_keyboard.borrow().clone();
                    if let Some(submit) = submit {
                        submit();
                    } else {
                        enter("\r");
                    }
                }))
                .child(key_chip("⌫", || false, move || backspace("\u{7f}")))
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
                        let paste_modes = paste_modes.clone();
                        spawn(async move {
                            let clip = clipboard_string().await;
                            let payload =
                                encode_paste(&clip, paste_modes().bracketed_paste);
                            if !payload.is_empty() {
                                paste(&payload);
                            }
                        });
                    },
                )),
        )
}

/// Pure state machine bridging native `EditText` / `UITextField` text changes,
/// IME compositions, and async seed resets into exact PTY byte streams.
struct InputBridge {
    committed: String,
    flushed_prop: String,
    pending: Option<String>,
    reseeded_from: Option<String>,
    composing: bool,
}

impl InputBridge {
    fn new() -> Self {
        let initial = INPUT_SEEDS[0].to_owned();
        Self {
            committed: initial.clone(),
            flushed_prop: initial,
            pending: None,
            reseeded_from: None,
            composing: false,
        }
    }

    fn note_flushed_prop(&mut self, value: &str) {
        self.flushed_prop.clear();
        self.flushed_prop.push_str(value);
    }

    /// Buffers `next` during IME composition, returning `true` when deferred.
    fn buffer_if_composing(&mut self, next: &str) -> bool {
        if self.composing {
            self.pending = Some(next.to_owned());
            true
        } else {
            false
        }
    }

    /// Updates composition state and returns any buffered edit to commit now
    /// that the composition has finished.
    fn set_composing(&mut self, composing: bool) -> Option<String> {
        self.composing = composing;
        if composing { None } else { self.pending.take() }
    }

    fn take_pending(&mut self) -> Option<String> {
        self.pending.take()
    }

    /// Diffs a committed native input value into PTY bytes and returns an
    /// optional replacement seed if the buffer needs to be re-seeded.
    fn commit(&mut self, next: &str) -> (String, Option<String>) {
        let base = match self.reseeded_from.take() {
            Some(old) if is_dropped_reseed_edit(&old, next) => old,
            _ => self.committed.clone(),
        };
        let data = terminal_edit(&base, next);
        self.committed = next.to_owned();
        if next.len() < INPUT_RESEED_LOW || next.len() > INPUT_RESEED_HIGH {
            (data, Some(self.reseed()))
        } else {
            (data, None)
        }
    }

    /// Clears any active composition and requests a fresh seed runway.
    fn reset(&mut self) -> String {
        self.composing = false;
        self.pending = None;
        self.reseed()
    }

    /// Picks a seed guaranteed to differ from `flushed_prop`, the current
    /// committed value, and the pre-reseed leading-space count so:
    /// - `nsh-core`'s prop differ always flushes `MountItem::InputValue`, and
    /// - if the platform still drops the async `setValue` (e.g. a faster edit
    ///   incremented `InputBox.count`), `reseeded_from` catches the stale text
    ///   without ever duplicating the previous command.
    fn reseed(&mut self) -> String {
        if self.reseeded_from.is_some()
            && self.committed != self.flushed_prop
            && INPUT_SEEDS.contains(&self.committed.as_str())
        {
            return self.committed.clone();
        }
        let previous = self.committed.clone();
        if self.reseeded_from.is_none() {
            self.reseeded_from = Some(previous.clone());
        }
        let anchor_spaces = leading_spaces(self.reseeded_from.as_deref().unwrap_or(&previous));
        let next_seed = INPUT_SEEDS
            .iter()
            .copied()
            .find(|&seed| {
                seed != self.flushed_prop && seed != previous && seed.len() != anchor_spaces
            })
            .or_else(|| {
                INPUT_SEEDS
                    .iter()
                    .copied()
                    .find(|&seed| seed != self.flushed_prop && seed != previous)
            })
            .unwrap_or(INPUT_SEEDS[0]);
        self.committed = next_seed.to_owned();
        next_seed.to_owned()
    }
}

fn leading_spaces(text: &str) -> usize {
    text.bytes().take_while(|&byte| byte == b' ').count()
}

/// Returns `true` when `next` is clearly an edit against the pre-reseed text
/// `old` (meaning the platform dropped or raced past our async `setValue`
/// reset) rather than fresh typing on top of the new seed runway.
fn is_dropped_reseed_edit(old: &str, next: &str) -> bool {
    let old_spaces = leading_spaces(old);
    if old_spaces == old.len() {
        return false;
    }
    // Same text re-reported, or new characters appended onto stale command text.
    if next.starts_with(old) {
        return true;
    }
    // Backspace deleting from stale command text while non-space text remains.
    if old.starts_with(next) && next.len() > old_spaces {
        return true;
    }
    false
}

fn terminal_keyboard(
    keyboard: ElementRef,
    active: Signal<Option<String>>,
    composing_text: Signal<Option<SharedString>>,
    submit_keyboard: SubmitHandle,
    modes: Rc<dyn Fn() -> EmulatorModes>,
    send: TerminalAction,
) -> impl IntoElement {
    // `input_value` mirrors `on_change` during normal typing and composition so
    // `nsh-core`'s prop differ stays in sync while `core::Prop::Value` sees
    // `input.value == value` and emits nothing to the native `EditText` (zero
    // mid-word `setText`/`setSelection` disruption). On re-seed/reset
    // `InputBridge` picks a distinct seed and moves the caret to the end.
    let input_value = signal(INPUT_SEEDS[0].to_owned());
    let bridge = Rc::new(RefCell::new(InputBridge::new()));
    let focused = signal(false);

    let apply_seed = move |next_seed: String| {
        let end = next_seed.encode_utf16().count() as u32;
        input_value.set(next_seed);
        keyboard.set_selection(end, end);
    };

    let reset = {
        let bridge = bridge.clone();
        move || {
            composing_text.set(None);
            let next_seed = bridge.borrow_mut().reset();
            apply_seed(next_seed);
        }
    };

    let previous_active = stored(active.get_untracked());
    effect({
        let reset = reset.clone();
        move || {
            let current = active.get();
            if previous_active.replace(current.clone()) != current {
                keyboard.blur();
                reset();
            }
        }
    });

    // Hardware keyboard shortcuts while focused (navigation + Ctrl/Alt combos).
    // Regular text, Backspace, and Enter flow through the native IME EditText
    // path (`on_change` / `on_submit`) so soft keyboards and hardware keyboards
    // never double-send characters.
    {
        let send = send.clone();
        let modes = modes.clone();
        effect(move || {
            if !focused.get() {
                return;
            }
            for &(key, seq) in HARDWARE_FIXED_KEYS {
                let send = send.clone();
                on_key_with(
                    key,
                    KeyOptions {
                        when: When::Always,
                        repeat: true,
                        for_focus: true,
                        ..Default::default()
                    },
                    move |_| send(seq),
                );
            }
            for &(key, code) in HARDWARE_CURSOR_KEYS {
                let send = send.clone();
                let modes = modes.clone();
                on_key_with(
                    key,
                    KeyOptions {
                        when: When::Always,
                        repeat: true,
                        for_focus: true,
                        ..Default::default()
                    },
                    move |_| {
                        let seq = if modes().application_cursor {
                            format!("\x1bO{code}")
                        } else {
                            format!("\x1b[{code}")
                        };
                        send(&seq);
                    },
                );
            }
        });
    }

    let commit = {
        let bridge = bridge.clone();
        let send = send.clone();
        move |next: &str| {
            let (data, reseed_target) = bridge.borrow_mut().commit(next);
            if let Some(next_seed) = reseed_target {
                apply_seed(next_seed);
            } else {
                input_value.set(next.to_owned());
            }
            if !data.is_empty() {
                send(&data);
            }
        }
    };

    let submit_action: Rc<dyn Fn()> = Rc::new({
        let bridge = bridge.clone();
        let commit = commit.clone();
        let reset = reset.clone();
        let send = send.clone();
        move || {
            let unflushed = bridge.borrow_mut().take_pending();
            if let Some(unflushed) = unflushed {
                commit(&unflushed);
            }
            send("\r");
            reset();
        }
    });
    *submit_keyboard.borrow_mut() = Some(submit_action.clone());
    {
        let submit_keyboard = submit_keyboard.clone();
        cleanup(move || {
            submit_keyboard.borrow_mut().take();
        });
    }

    let on_paste_send = send.clone();
    let on_paste_modes = modes.clone();
    let value_bridge = bridge.clone();

    input()
        .element_ref(&keyboard)
        .absolute()
        // At the foot of the grid, where a shell's prompt is, so an input
        // method opens its candidates near the line being typed.
        .bottom_0()
        .left_0()
        .size(px(theme::TOUCH_TARGET))
        // Opaque, with transparent ink: the field draws nothing, and no
        // platform mistakes it for a hidden decorative layer. `pointer_events`
        // keeps it out of the way of touches on the terminal grid.
        .opacity(1.0)
        .text_color(theme::foreground().opacity(0.0))
        .caret(false)
        .pointer_events(PointerEvents::None)
        .label(copy::text("Terminal input", "终端输入", "終端機輸入"))
        .auto_correct(false)
        .auto_capitalize(AutoCapitalize::None)
        .auto_fill(AutoFill::Off)
        .paste_behavior(PasteBehavior::Report)
        .submit_behavior(SubmitBehavior::Submit)
        .value(move || {
            let value = input_value.get();
            value_bridge.borrow_mut().note_flushed_prop(&value);
            value
        })
        .selected_range((
            INPUT_SEEDS[0].encode_utf16().count() as u32,
            INPUT_SEEDS[0].encode_utf16().count() as u32,
        ))
        .on_focus(move || focused.set(true))
        .on_change({
            let bridge = bridge.clone();
            let commit = commit.clone();
            move |next: &str| {
                if bridge.borrow_mut().buffer_if_composing(next) {
                    input_value.set(next.to_owned());
                } else {
                    commit(next);
                }
            }
        })
        .on_composition_change({
            let bridge = bridge.clone();
            let commit = commit.clone();
            move |composition: &Composition| {
                let flushed = bridge.borrow_mut().set_composing(composition.composing);
                composing_text.set(if composition.composing && !composition.text.is_empty() {
                    Some(SharedString::from(composition.text.clone()))
                } else {
                    None
                });
                if let Some(next) = flushed {
                    commit(&next);
                }
            }
        })
        .on_paste(move |event: &PasteEvent| {
            let payload = encode_paste(&event.text, on_paste_modes().bracketed_paste);
            if !payload.is_empty() {
                on_paste_send(&payload);
            }
        })
        .on_submit(move |_: &str| submit_action())
        .on_blur(move || {
            focused.set(false);
            reset();
        })
}

const HARDWARE_CURSOR_KEYS: &[(&str, char)] = &[
    ("up", 'A'),
    ("down", 'B'),
    ("right", 'C'),
    ("left", 'D'),
    ("home", 'H'),
    ("end", 'F'),
];

const HARDWARE_FIXED_KEYS: &[(&str, &str)] = &[
    ("tab", "\t"),
    ("shift+tab", "\x1b[Z"),
    ("escape", "\x1b"),
    ("delete", "\x1b[3~"),
    ("pageup", "\x1b[5~"),
    ("pagedown", "\x1b[6~"),
    ("alt+left", "\x1bb"),
    ("alt+right", "\x1bf"),
    ("alt+backspace", "\x1b\x7f"),
    ("ctrl+a", "\x01"),
    ("ctrl+b", "\x02"),
    ("ctrl+c", "\x03"),
    ("ctrl+d", "\x04"),
    ("ctrl+e", "\x05"),
    ("ctrl+f", "\x06"),
    ("ctrl+g", "\x07"),
    ("ctrl+h", "\x08"),
    ("ctrl+i", "\x09"),
    ("ctrl+j", "\x0a"),
    ("ctrl+k", "\x0b"),
    ("ctrl+l", "\x0c"),
    ("ctrl+m", "\x0d"),
    ("ctrl+n", "\x0e"),
    ("ctrl+o", "\x0f"),
    ("ctrl+p", "\x10"),
    ("ctrl+r", "\x12"),
    ("ctrl+s", "\x13"),
    ("ctrl+t", "\x14"),
    ("ctrl+u", "\x15"),
    ("ctrl+v", "\x16"),
    ("ctrl+w", "\x17"),
    ("ctrl+x", "\x18"),
    ("ctrl+y", "\x19"),
    ("ctrl+z", "\x1a"),
];

fn encode_paste(text: &str, bracketed: bool) -> String {
    let normalized = terminal_newlines(text);
    if normalized.is_empty() {
        return normalized;
    }
    if bracketed {
        format!("\x1b[200~{normalized}\x1b[201~")
    } else {
        normalized
    }
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
        .h(px(TERMINAL_KEY_HEIGHT))
        .min_w(px(42.0))
        .px(px(10.0))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .bg(move || {
            if selected() {
                theme::terminal_key_active()
            } else {
                theme::terminal_key()
            }
        })
        .active(|style| style.opacity(0.6))
        .on_press(action)
        .child(
            text(label)
                .font_family(theme::TERMINAL_FONT_FAMILY)
                .text_size(px(theme::font_micro()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || {
                    if selected() {
                        theme::primary_foreground()
                    } else {
                        theme::terminal_key_label()
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
    use super::{INPUT_SEEDS, InputBridge, encode_paste, modified_input, terminal_edit};

    #[test]
    fn native_input_edits_produce_pty_bytes() {
        assert_eq!(terminal_edit("        ", "        你好"), "你好");
        assert_eq!(terminal_edit("        你好", "        你"), "\u{7f}");
        assert_eq!(terminal_edit("        ", "       "), "\u{7f}");
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

    #[test]
    fn bracketed_paste_wraps_payload_only_when_enabled() {
        assert_eq!(encode_paste("echo hi\n", false), "echo hi\r");
        assert_eq!(
            encode_paste("echo hi\n", true),
            "\x1b[200~echo hi\r\x1b[201~"
        );
    }

    #[test]
    fn same_tick_command_and_submit_always_picks_a_new_seed_prop() {
        let mut bridge = InputBridge::new();
        // Initial prop flushed by nsh-core is INPUT_SEEDS[0].
        bridge.note_flushed_prop(INPUT_SEEDS[0]);

        // Typing "pwd" and submitting in the same tick (before prop flush):
        let (typed, reseed) = bridge.commit(&format!("{}pwd", INPUT_SEEDS[0]));
        assert_eq!(typed, "pwd");
        assert!(reseed.is_none());

        let next_seed = bridge.reset();
        assert_ne!(
            next_seed, INPUT_SEEDS[0],
            "reseed must differ from the last flushed prop so nsh-core emits InputValue"
        );
        bridge.note_flushed_prop(&next_seed);

        // Next command "ls" after the native field applied `next_seed`:
        let (l, _) = bridge.commit(&format!("{next_seed}l"));
        let (s, _) = bridge.commit(&format!("{next_seed}ls"));
        assert_eq!(format!("{l}{s}"), "ls");
    }

    #[test]
    fn dropped_native_reset_never_replays_previous_command() {
        let mut bridge = InputBridge::new();
        bridge.note_flushed_prop(INPUT_SEEDS[0]);

        let prev_text = format!("{}pwd", INPUT_SEEDS[0]);
        assert_eq!(bridge.commit(&prev_text).0, "pwd");
        bridge.note_flushed_prop(&prev_text);

        // Submit resets the bridge, requesting a new seed from Android...
        let _next_seed = bridge.reset();

        // ...but Android drops the async `setValue` (e.g. count raced ahead)
        // so the native EditText still contains `        pwd` when user types `ls`:
        let (l, _) = bridge.commit(&format!("{prev_text}l"));
        let (s, _) = bridge.commit(&format!("{prev_text}ls"));
        assert_eq!(
            format!("{l}{s}"),
            "ls",
            "must send only `ls`, never replay `pwdls`"
        );
    }

    #[test]
    fn dropped_native_reset_handles_composing_and_backspace_cleanly() {
        let mut bridge = InputBridge::new();
        bridge.note_flushed_prop(INPUT_SEEDS[0]);

        let prev_text = format!("{}pwd", INPUT_SEEDS[0]);
        assert_eq!(bridge.commit(&prev_text).0, "pwd");
        bridge.note_flushed_prop(&prev_text);
        let _next_seed = bridge.reset();

        // Composing "ls" on top of stale EditText text:
        assert!(bridge.set_composing(true).is_none());
        assert!(bridge.buffer_if_composing(&format!("{prev_text}l")));
        assert!(bridge.buffer_if_composing(&format!("{prev_text}ls")));
        let flushed = bridge.set_composing(false).unwrap();
        assert_eq!(bridge.commit(&flushed).0, "ls");

        // Another dropped reset followed by Backspace:
        let stale = format!("{prev_text}ls");
        let _next_seed = bridge.reset();
        assert_eq!(bridge.commit(&format!("{prev_text}l")).0, "\u{7f}");
        assert_eq!(bridge.commit(&stale).0, "s");
    }

    #[test]
    fn repeating_identical_command_after_applied_reset_still_sends_it() {
        let mut bridge = InputBridge::new();
        bridge.note_flushed_prop(INPUT_SEEDS[0]);

        let first = format!("{}pwd", INPUT_SEEDS[0]);
        assert_eq!(bridge.commit(&first).0, "pwd");
        bridge.note_flushed_prop(&first);

        let next_seed = bridge.reset();
        bridge.note_flushed_prop(&next_seed);

        // Native field applied `next_seed` (different leading-space length),
        // then user inserts "pwd" again in a single candidate commit:
        let second = format!("{next_seed}pwd");
        assert_eq!(bridge.commit(&second).0, "pwd");
    }
}

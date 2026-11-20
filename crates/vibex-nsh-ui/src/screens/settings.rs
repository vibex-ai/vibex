//! The settings sheet: the app's small set of device preferences.
//!
//! The GPUI client keeps a full Settings centre; this sheet carries the rows
//! the `nsh` client can act on today — appearance, language, the connected
//! runtime and the About facts — with the same wording and order the phone
//! uses. The values live in the app's settings store, so they survive a
//! restart, and the sheet is reachable both from the pairing page and from the
//! session drawer.

use std::rc::Rc;

use nsh_base::{SheetEvent, sheet};
use nsh_core::{element::ImageSrc, platform::Appearance, prelude::*};

use crate::{copy, theme, ui::action};

/// What the settings sheet may do, and the state it shows.
#[derive(Clone)]
pub struct SettingsHost {
    pub open: Signal<bool>,
    /// The appearance the app is pinned to, or `System`.
    pub appearance: Signal<Appearance>,
    /// The language the app is pinned to, or `System`.
    pub language: Signal<copy::LanguagePreference>,
    /// Whether finished actions play the device's feedback.
    pub haptics: Signal<bool>,
    /// Whether a running turn or the terminal holds the display awake.
    pub keep_screen_on: Signal<bool>,
    /// The text-size step, as a percentage of the ladder.
    pub font_scale: Signal<u32>,
    /// Whether the app skips its own motion.
    pub reduce_motion: Signal<bool>,
    /// The notification master switch and its three categories.
    pub notifications: Signal<bool>,
    pub notify_completed: Signal<bool>,
    pub notify_needs_input: Signal<bool>,
    pub notify_failed: Signal<bool>,
    /// The conversation chrome the reader wants to see.
    pub show_token_usage: Signal<bool>,
    pub show_plan_goal: Signal<bool>,
    pub show_generation_status: Signal<bool>,
    pub show_reasoning: Signal<bool>,
    pub auto_continue: Signal<bool>,
    /// The connected runtime's label, when there is one.
    pub runtime: Signal<Option<SharedString>>,
    /// The runtimes this phone saved, for the runtime actions.
    pub saved: Signal<Vec<crate::screens::pairing::SavedRuntime>>,
    pub on_appearance: Rc<dyn Fn(Appearance)>,
    pub on_language: Rc<dyn Fn(copy::LanguagePreference)>,
    pub on_haptics: Rc<dyn Fn(bool)>,
    pub on_keep_screen_on: Rc<dyn Fn(bool)>,
    pub on_font_scale: Rc<dyn Fn(u32)>,
    pub on_reduce_motion: Rc<dyn Fn(bool)>,
    pub on_notifications: Rc<dyn Fn(bool)>,
    pub on_notify_completed: Rc<dyn Fn(bool)>,
    pub on_notify_needs_input: Rc<dyn Fn(bool)>,
    pub on_notify_failed: Rc<dyn Fn(bool)>,
    pub on_show_token_usage: Rc<dyn Fn(bool)>,
    pub on_show_plan_goal: Rc<dyn Fn(bool)>,
    pub on_show_generation_status: Rc<dyn Fn(bool)>,
    pub on_show_reasoning: Rc<dyn Fn(bool)>,
    pub on_auto_continue: Rc<dyn Fn(bool)>,
    pub on_disconnect: Rc<dyn Fn()>,
    pub on_reconnect: Rc<dyn Fn()>,
    pub on_switch_runtime: Rc<dyn Fn()>,
    pub on_clear_runtimes: Rc<dyn Fn()>,
    pub on_close: Rc<dyn Fn()>,
}

/// The sheet.
pub fn settings(host: SettingsHost) -> impl IntoElement {
    let open = host.open;
    let on_close = host.on_close.clone();
    let on_close_sheet = on_close.clone();
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event
                && !next
            {
                on_close_sheet();
            }
        })
        .child(
            scroll().max_h(px(600.0)).child(
                view()
                    .w_full()
                    .gap(px(theme::SPACING_MD))
                    .p(px(theme::SPACING_MD))
                    .pb_safe_area_plus(px(theme::SPACING_LG))
                    .bg(theme::background_card)
                    .rounded_t(px(16.0))
                    .child(
                        text(copy::text("Settings", "设置", "設定"))
                            .text_size(px(theme::font_heading()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                    )
                    .child(runtime_section(host.clone()))
                    .child(section(
                        copy::text("Appearance", "外观", "外觀"),
                        appearance_choices(host.appearance, host.on_appearance.clone()),
                    ))
                    .child(section(
                        copy::text("Language", "语言", "語言"),
                        language_choices(host.language, host.on_language.clone()),
                    ))
                    .child(section(
                        copy::text("Text", "文字", "文字"),
                        text_size_row(host.font_scale, host.on_font_scale.clone()),
                    ))
                    .child(section(
                        copy::text("Notifications", "通知", "通知"),
                        view()
                            .w_full()
                            .flex_col()
                            .gap(px(theme::SPACING_XS))
                            .child(toggle_row(
                                copy::text("Enable notifications", "启用通知", "啟用通知"),
                                host.notifications,
                                host.on_notifications.clone(),
                            ))
                            .child(toggle_row(
                                copy::text("Agent completed", "Agent 已完成", "Agent 已完成"),
                                host.notify_completed,
                                host.on_notify_completed.clone(),
                            ))
                            .child(toggle_row(
                                copy::text("Needs input", "需要输入", "需要輸入"),
                                host.notify_needs_input,
                                host.on_notify_needs_input.clone(),
                            ))
                            .child(toggle_row(
                                copy::text("Failed", "执行失败", "執行失敗"),
                                host.notify_failed,
                                host.on_notify_failed.clone(),
                            )),
                    ))
                    .child(section(
                        copy::text("Conversation", "会话", "工作階段"),
                        view()
                            .w_full()
                            .flex_col()
                            .gap(px(theme::SPACING_XS))
                            .child(toggle_row(
                                copy::text(
                                    "Show token usage",
                                    "显示 Token 使用量",
                                    "顯示 Token 使用量",
                                ),
                                host.show_token_usage,
                                host.on_show_token_usage.clone(),
                            ))
                            .child(toggle_row(
                                copy::text(
                                    "Show plan and goal",
                                    "显示计划与目标",
                                    "顯示計畫與目標",
                                ),
                                host.show_plan_goal,
                                host.on_show_plan_goal.clone(),
                            ))
                            .child(toggle_row(
                                copy::text(
                                    "Show generation status",
                                    "显示生成状态",
                                    "顯示生成狀態",
                                ),
                                host.show_generation_status,
                                host.on_show_generation_status.clone(),
                            ))
                            .child(toggle_row(
                                copy::text("Show reasoning", "显示思考过程", "顯示思考過程"),
                                host.show_reasoning,
                                host.on_show_reasoning.clone(),
                            ))
                            .child(toggle_row(
                                copy::text(
                                    "Continue paused turns",
                                    "自动继续暂停的回合",
                                    "自動繼續暫停的回合",
                                ),
                                host.auto_continue,
                                host.on_auto_continue.clone(),
                            )),
                    ))
                    .child(section(
                        copy::text("Feedback", "反馈", "回饋"),
                        view()
                            .w_full()
                            .flex_col()
                            .gap(px(theme::SPACING_XS))
                            .child(toggle_row(
                                copy::text("Haptic feedback", "触感反馈", "觸感回饋"),
                                host.haptics,
                                host.on_haptics.clone(),
                            ))
                            .child(toggle_row(
                                copy::text("Keep the screen on", "保持屏幕常亮", "保持螢幕常亮"),
                                host.keep_screen_on,
                                host.on_keep_screen_on.clone(),
                            )),
                    ))
                    .child(section(
                        copy::text("About", "关于", "關於"),
                        view()
                            .w_full()
                            .gap(px(theme::SPACING_XS))
                            .child(value_row(copy::text("Version", "版本", "版本"), || {
                                concat!("Vibex ", env!("CARGO_PKG_VERSION")).to_owned()
                            }))
                            .child(value_row(copy::text("License", "许可", "授權"), || {
                                "AGPL-3.0-or-later".to_owned()
                            })),
                    ))
                    .child(when(move || !host.saved.with(|saved| saved.is_empty()), {
                        let on_clear = host.on_clear_runtimes.clone();
                        move || {
                            let on_clear = on_clear.clone();
                            action::secondary(
                                copy::text(
                                    "Remove saved runtimes",
                                    "移除已保存的运行时",
                                    "移除已儲存的執行環境",
                                ),
                                icon(include_str!("../../assets/icons/x.svg")),
                                false,
                                move || on_clear(),
                            )
                            .w_full()
                        }
                    }))
                    .child({
                        let on_close = on_close.clone();
                        action::secondary(
                            copy::text("Close", "关闭", "關閉"),
                            icon(include_str!("../../assets/icons/x.svg")),
                            false,
                            move || on_close(),
                        )
                        .w_full()
                    }),
            ),
        )
}

fn runtime_section(host: SettingsHost) -> impl IntoElement {
    let runtime = host.runtime;
    let on_disconnect = host.on_disconnect.clone();
    view()
        .w_full()
        .gap(px(theme::SPACING_XS))
        .child(section_label(copy::text("Runtime", "运行时", "執行環境")))
        .child(value_row(
            copy::text("Server", "服务器", "伺服器"),
            move || {
                runtime
                    .get()
                    .map(|label| label.to_string())
                    .unwrap_or_else(|| copy::text("Not connected", "未连接", "未連線").to_owned())
            },
        ))
        .child(when(move || runtime.get().is_some(), {
            let on_reconnect = host.on_reconnect.clone();
            let on_switch = host.on_switch_runtime.clone();
            let on_disconnect = on_disconnect.clone();
            move || {
                view()
                    .w_full()
                    .flex_col()
                    .gap(px(theme::SPACING_XS))
                    .child(
                        action::secondary(
                            copy::text("Reconnect", "重新连接", "重新連線"),
                            icon(include_str!("../../assets/icons/refresh.svg")),
                            false,
                            {
                                let on_reconnect = on_reconnect.clone();
                                move || on_reconnect()
                            },
                        )
                        .w_full(),
                    )
                    .child(
                        action::secondary(
                            copy::text("Switch runtime", "切换运行时", "切換執行環境"),
                            icon(include_str!("../../assets/icons/refresh.svg")),
                            false,
                            {
                                let on_switch = on_switch.clone();
                                move || on_switch()
                            },
                        )
                        .w_full(),
                    )
                    .child(
                        action::secondary(
                            copy::text("Disconnect", "断开连接", "中斷連線"),
                            icon(include_str!("../../assets/icons/plug-zap.svg")),
                            false,
                            {
                                let on_disconnect = on_disconnect.clone();
                                move || on_disconnect()
                            },
                        )
                        .w_full(),
                    )
            }
        }))
}

fn appearance_choices(
    appearance: Signal<Appearance>,
    on_appearance: Rc<dyn Fn(Appearance)>,
) -> impl IntoElement {
    let choices = [
        (
            Appearance::System,
            copy::text("System", "跟随系统", "跟隨系統"),
        ),
        (Appearance::Light, copy::text("Light", "浅色", "淺色")),
        (Appearance::Dark, copy::text("Dark", "深色", "深色")),
    ];
    view()
        .flex_row()
        .w_full()
        .gap(px(theme::SPACING_SM))
        .children(choices.into_iter().map({
            let on_appearance = on_appearance.clone();
            move |(choice, label)| {
                let on_appearance = on_appearance.clone();
                action::secondary(
                    label,
                    icon(include_str!("../../assets/icons/image.svg")),
                    appearance.with(|current| *current == choice),
                    move || on_appearance(choice),
                )
                .flex_1()
            }
        }))
}

fn language_choices(
    language: Signal<copy::LanguagePreference>,
    on_language: Rc<dyn Fn(copy::LanguagePreference)>,
) -> impl IntoElement {
    use copy::LanguagePreference as Choice;
    let choices = [
        (Choice::System, copy::text("System", "跟随系统", "跟隨系統")),
        (Choice::English, "English"),
        (Choice::SimplifiedChinese, "简体中文"),
        (Choice::TraditionalChinese, "繁體中文"),
    ];
    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .children(choices.into_iter().map({
            let on_language = on_language.clone();
            move |(choice, label)| {
                let on_language = on_language.clone();
                action::secondary(
                    label,
                    icon(include_str!("../../assets/icons/check.svg")),
                    language.with(|current| *current == choice),
                    move || on_language(choice),
                )
                .w_full()
            }
        }))
}

fn text_size_row(percent: Signal<u32>, on_font_scale: Rc<dyn Fn(u32)>) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .child(view().flex_1().child(value_row(
            copy::text("Text size", "文字大小", "文字大小"),
            move || font_scale_label(percent.get()).to_owned(),
        )))
        .child(action::secondary(
            copy::text("Change", "调整", "調整"),
            icon(include_str!("../../assets/icons/check.svg")),
            false,
            move || on_font_scale(theme::next_font_scale_percent(percent.get_untracked())),
        ))
}

fn font_scale_label(percent: u32) -> &'static str {
    match percent {
        80 => copy::text("Smallest", "最小", "最小"),
        90 => copy::text("Compact", "紧凑", "緊湊"),
        100 => copy::text("Standard", "标准", "標準"),
        115 => copy::text("Large", "较大", "較大"),
        _ => copy::text("Standard", "标准", "標準"),
    }
}

fn toggle_row(
    label: &'static str,
    enabled: Signal<bool>,
    on_toggle: Rc<dyn Fn(bool)>,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .child(view().flex_1().child(value_row(label, move || {
            if enabled.get() {
                copy::text("On", "开", "開").to_owned()
            } else {
                copy::text("Off", "关", "關").to_owned()
            }
        })))
        .child(
            action::secondary(
                copy::text("Toggle", "切换", "切換"),
                icon(include_str!("../../assets/icons/check.svg")),
                false,
                move || on_toggle(!enabled.get_untracked()),
            )
            .accessibility_label(label),
        )
}

fn section(label: &'static str, body: impl IntoElement) -> impl IntoElement {
    view()
        .w_full()
        .gap(px(theme::SPACING_XS))
        .child(section_label(label))
        .child(body)
}

fn section_label(label: &'static str) -> impl IntoElement {
    text(label)
        .text_size(px(theme::font_micro()))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted)
}

fn value_row(label: &'static str, value: impl Fn() -> String + 'static) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(theme::SPACING_SM))
        .child(
            text(label)
                .text_size(px(theme::font_detail()))
                .text_color(theme::foreground),
        )
        .child(
            text(value)
                .truncate()
                .text_size(px(theme::font_detail()))
                .text_color(theme::text_muted),
        )
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

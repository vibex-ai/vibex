//! The settings page: the app's device preferences, as one full-screen surface.
//!
//! The page is a calm, scrollable column of grouped cards rather than a long
//! sheet of buttons: each card names its topic, and every control is the
//! control the value deserves — a segmented appearance choice, a stepper for
//! text size, switches for the things that are on or off, and rows that lead
//! somewhere for the rest. The values live in the app's settings store, so they
//! survive a restart, and the page is reachable both from the pairing page and
//! from the session drawer.

use std::rc::Rc;

use nsh_base::button;
use nsh_core::{element::ImageSrc, platform::Appearance, prelude::*};

use crate::{copy, theme, ui::action};

/// The page's outer gutter.
const PAGE_GUTTER: f32 = 20.0;
/// The gap between two cards.
const CARD_GAP: f32 = 18.0;
/// The corner radius of a card on this page.
const CARD_RADIUS: f32 = 14.0;
/// The height of the page's title bar.
const HEADER_HEIGHT: f32 = 60.0;

/// What the settings page may do, and the state it shows.
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
    pub on_install_update: Rc<dyn Fn(&str)>,
    pub on_disconnect: Rc<dyn Fn()>,
    pub on_reconnect: Rc<dyn Fn()>,
    pub on_switch_runtime: Rc<dyn Fn()>,
    pub on_clear_runtimes: Rc<dyn Fn()>,
    pub on_close: Rc<dyn Fn()>,
}

/// The settings page, presented over whatever opened it.
pub fn settings(host: SettingsHost) -> impl IntoElement {
    let open = host.open;
    dynamic(move || {
        if open.get() {
            page(host.clone()).into_element()
        } else {
            view().into_element()
        }
    })
}

fn page(host: SettingsHost) -> impl IntoElement {
    let update_status = signal(None::<SharedString>);
    let update_url = signal(None::<String>);
    view()
        .absolute()
        .inset(px(0.0))
        .z_index(30)
        .bg(theme::background)
        .flex_col()
        .child(header(host.clone()))
        .child(
            view().flex_1().flex_col().child(
                scroll().flex_1().child(
                    view()
                        .w_full()
                        .flex_col()
                        .gap(px(CARD_GAP))
                        .px(px(PAGE_GUTTER))
                        .pt(px(theme::SPACING_MD))
                        .pb_safe_area_plus(px(theme::SPACING_XL))
                        .child(runtime_card(host.clone()))
                        .child(appearance_card(host.clone()))
                        .child(language_card(host.clone()))
                        .child(conversation_card(host.clone()))
                        .child(notifications_card(host.clone()))
                        .child(device_card(host.clone()))
                        .child(about_card(host.clone(), update_status, update_url))
                        .child(when(move || !host.saved.with(|saved| saved.is_empty()), {
                            let host = host.clone();
                            move || danger_card(host.clone())
                        }))
                        .child(
                            text(format!(
                                "Vibex {} · AGPL-3.0-or-later",
                                env!("CARGO_PKG_VERSION")
                            ))
                            .w_full()
                            .text_center()
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted),
                        ),
                ),
            ),
        )
}

fn header(host: SettingsHost) -> impl IntoElement {
    let on_close = host.on_close.clone();
    view().pt_safe_area().child(
        view()
            .h(px(HEADER_HEIGHT))
            .flex_shrink_0()
            .px(px(PAGE_GUTTER))
            .flex_row()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_b_color(theme::border_subtle)
            .child(
                text(copy::text("Settings", "设置", "設定"))
                    .text_size(px(theme::font_title()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::foreground),
            )
            .child(
                button()
                    .size(px(theme::TOUCH_TARGET))
                    .items_center()
                    .justify_center()
                    .active(|style| style.opacity(0.65))
                    .accessibility_label(copy::text("Close", "关闭", "關閉"))
                    .on_press(move || on_close())
                    .child(
                        svg(icon(include_str!("../../assets/icons/x.svg")))
                            .w(px(theme::ICON_MD))
                            .h(px(theme::ICON_MD))
                            .text_color(theme::text_muted),
                    ),
            ),
    )
}

/// The runtime at the top: who this phone is talking to, and how to leave it.
fn runtime_card(host: SettingsHost) -> impl IntoElement {
    let runtime = host.runtime;
    let saved = host.saved;
    let connected = move || runtime.get().is_some();
    let actions = {
        let on_reconnect = host.on_reconnect.clone();
        let on_switch = host.on_switch_runtime.clone();
        let on_disconnect = host.on_disconnect.clone();
        move || {
            view()
                .flex_row()
                .w_full()
                .gap(px(theme::SPACING_SM))
                .child(compact_action(
                    copy::text("Reconnect", "重新连接", "重新連線"),
                    icon(include_str!("../../assets/icons/refresh.svg")),
                    false,
                    {
                        let on_reconnect = on_reconnect.clone();
                        move || on_reconnect()
                    },
                ))
                .child(compact_action(
                    copy::text("Switch", "切换", "切換"),
                    icon(include_str!("../../assets/icons/chevron-right.svg")),
                    false,
                    {
                        let on_switch = on_switch.clone();
                        move || on_switch()
                    },
                ))
                .child(compact_action(
                    copy::text("Disconnect", "断开", "中斷"),
                    icon(include_str!("../../assets/icons/plug-zap.svg")),
                    true,
                    {
                        let on_disconnect = on_disconnect.clone();
                        move || on_disconnect()
                    },
                ))
        }
    };
    view()
        .w_full()
        .rounded(px(16.0))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_card)
        .p(px(theme::SPACING_LG))
        .flex_col()
        .gap(px(theme::SPACING_MD))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_MD))
                .child(icon_tile(
                    icon(include_str!("../../assets/icons/monitor.svg")),
                    false,
                ))
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            text(move || {
                                if connected() {
                                    copy::text(
                                        "Connected runtime",
                                        "已连接运行时",
                                        "已連線執行環境",
                                    )
                                } else {
                                    copy::text("No runtime", "未连接运行时", "尚未連線執行環境")
                                }
                            })
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted),
                        )
                        .child(
                            text(move || {
                                runtime
                                    .get()
                                    .map(|label| label.to_string())
                                    .unwrap_or_else(|| {
                                        copy::text(
                                            "Pair one to start a session",
                                            "配对后即可开始会话",
                                            "配對後即可開始工作階段",
                                        )
                                        .to_owned()
                                    })
                            })
                            .truncate()
                            .text_size(px(theme::font_heading()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                        ),
                )
                .child(
                    view()
                        .w(px(8.0))
                        .h(px(8.0))
                        .flex_shrink_0()
                        .rounded(px(4.0))
                        .bg(move || {
                            if connected() {
                                theme::color_token("chart-1")
                            } else {
                                theme::text_muted()
                            }
                        }),
                ),
        )
        .child(when(connected, actions))
        .child(when(move || !connected(), {
            let on_close = host.on_close.clone();
            move || {
                let on_close = on_close.clone();
                action::primary(
                    copy::text("Find a runtime", "查找运行时", "尋找執行環境"),
                    icon(include_str!("../../assets/icons/chevron-right.svg")),
                    move || on_close(),
                )
                .w_full()
            }
        }))
        .child(when(
            move || saved.with(|saved| saved.len() > 1),
            move || {
                text(move || {
                    format!(
                        "{} {}",
                        saved.with(|saved| saved.len()),
                        copy::text("saved runtimes", "个已保存运行时", "個已儲存執行環境")
                    )
                })
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted)
            },
        ))
}

fn appearance_card(host: SettingsHost) -> impl IntoElement {
    let appearance = host.appearance;
    let font_scale = host.font_scale;
    let reduce_motion = host.reduce_motion;
    group_card(
        icon(include_str!("../../assets/icons/palette.svg")),
        copy::text("Appearance", "外观", "外觀"),
        view()
            .w_full()
            .flex_col()
            .child(
                row_shell()
                    .child(label_block(
                        copy::text("Theme", "主题", "主題"),
                        copy::text(
                            "Follow the system or pin the app's surface",
                            "跟随系统，或固定应用的明暗外观",
                            "跟隨系統，或固定應用程式的明暗外觀",
                        ),
                    ))
                    .child(value_text(move || {
                        appearance_label(appearance.get()).to_owned()
                    })),
            )
            .child(
                view()
                    .w_full()
                    .px(px(theme::SPACING_LG))
                    .pb(px(theme::SPACING_MD))
                    .child(appearance_segments(appearance, host.on_appearance.clone())),
            )
            .child(divider())
            .child(
                row_shell()
                    .child(label_block(
                        copy::text("Text size", "文字大小", "文字大小"),
                        copy::text(
                            "Scales copy and spacing across the app",
                            "缩放应用内的文字与间距",
                            "縮放應用程式內的文字與間距",
                        ),
                    ))
                    .child(text_size_stepper(font_scale, host.on_font_scale.clone())),
            )
            .child(
                view()
                    .w_full()
                    .px(px(theme::SPACING_LG))
                    .pb(px(theme::SPACING_MD))
                    .child(
                        view()
                            .w_full()
                            .rounded(px(10.0))
                            .bg(theme::background_dim)
                            .p(px(theme::SPACING_MD))
                            .child(
                                text(move || format!("Aa  {}", font_scale_label(font_scale.get())))
                                    .text_size(move || {
                                        px(font_scale.get() as f32 / 100.0 * theme::FONT_BODY)
                                    })
                                    .text_color(theme::text_secondary),
                            ),
                    ),
            )
            .child(divider())
            .child(toggle_row(
                copy::text("Reduce motion", "减少动效", "減少動效"),
                copy::text(
                    "Skip page and sheet transitions",
                    "跳过页面与弹层的过渡动画",
                    "跳過頁面與彈層的轉場動畫",
                ),
                reduce_motion,
                host.on_reduce_motion.clone(),
                None,
            )),
    )
}

fn language_card(host: SettingsHost) -> impl IntoElement {
    use copy::LanguagePreference as Choice;
    let language = host.language;
    let choices = [
        (
            Choice::System,
            copy::text("System", "跟随系统", "跟隨系統"),
            copy::text(
                "Use the phone's language",
                "使用手机的系统语言",
                "使用手機的系統語言",
            ),
        ),
        (Choice::English, "English", ""),
        (Choice::SimplifiedChinese, "简体中文", ""),
        (Choice::TraditionalChinese, "繁體中文", ""),
    ];
    let rows = choices.into_iter().enumerate().map({
        let on_language = host.on_language.clone();
        move |(index, (choice, label, detail))| {
            let on_language = on_language.clone();
            view()
                .w_full()
                .flex_col()
                .child(when(index > 0, divider))
                .child(
                    button()
                        .w_full()
                        .min_h(px(54.0))
                        .px(px(theme::SPACING_LG))
                        .py(px(theme::SPACING_SM))
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_MD))
                        .active(|style| style.opacity(0.75))
                        .on_press(move || on_language(choice))
                        .child(
                            view()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex_col()
                                .child(
                                    text(label)
                                        .text_size(px(theme::font_body()))
                                        .text_color(theme::foreground),
                                )
                                .child(when(
                                    move || !detail.is_empty(),
                                    move || {
                                        text(detail)
                                            .text_size(px(theme::font_micro()))
                                            .text_color(theme::text_muted)
                                    },
                                )),
                        )
                        .child(
                            view()
                                .w(px(theme::ICON_SM))
                                .h(px(theme::ICON_SM))
                                .items_center()
                                .child(when(
                                    move || language.with(|current| *current == choice),
                                    move || {
                                        svg(icon(include_str!("../../assets/icons/check.svg")))
                                            .w(px(theme::ICON_SM))
                                            .h(px(theme::ICON_SM))
                                            .text_color(theme::foreground)
                                    },
                                )),
                        ),
                )
        }
    });
    group_card(
        icon(include_str!("../../assets/icons/globe.svg")),
        copy::text("Language", "语言", "語言"),
        view().w_full().flex_col().children(rows),
    )
}

fn conversation_card(host: SettingsHost) -> impl IntoElement {
    group_card(
        icon(include_str!("../../assets/icons/message-square.svg")),
        copy::text("Conversation", "会话", "工作階段"),
        view()
            .w_full()
            .flex_col()
            .child(toggle_row(
                copy::text("Token usage", "Token 使用量", "Token 使用量"),
                copy::text(
                    "Show the session's token counters above the composer",
                    "在输入框上方显示本次会话的 Token 用量",
                    "在輸入框上方顯示本次工作階段的 Token 用量",
                ),
                host.show_token_usage,
                host.on_show_token_usage.clone(),
                None,
            ))
            .child(divider())
            .child(toggle_row(
                copy::text("Plan and goal", "计划与目标", "計畫與目標"),
                copy::text(
                    "Show the plan and goal bars the runtime publishes",
                    "显示运行时提供的计划与目标",
                    "顯示執行環境提供的計畫與目標",
                ),
                host.show_plan_goal,
                host.on_show_plan_goal.clone(),
                None,
            ))
            .child(divider())
            .child(toggle_row(
                copy::text("Generation status", "生成状态", "生成狀態"),
                copy::text(
                    "Show a live line while the Agent works",
                    "Agent 工作时显示实时状态行",
                    "Agent 工作時顯示即時狀態列",
                ),
                host.show_generation_status,
                host.on_show_generation_status.clone(),
                None,
            ))
            .child(divider())
            .child(toggle_row(
                copy::text("Reasoning", "思考过程", "思考過程"),
                copy::text(
                    "Show the Agent's reasoning rows",
                    "显示 Agent 的推理内容",
                    "顯示 Agent 的推理內容",
                ),
                host.show_reasoning,
                host.on_show_reasoning.clone(),
                None,
            ))
            .child(divider())
            .child(toggle_row(
                copy::text(
                    "Continue paused turns",
                    "自动继续暂停的回合",
                    "自動繼續暫停的回合",
                ),
                copy::text(
                    "Resume a turn that paused instead of waiting for a tap",
                    "回合暂停后自动继续，无需手动点击",
                    "回合暫停後自動繼續，無需手動點擊",
                ),
                host.auto_continue,
                host.on_auto_continue.clone(),
                None,
            )),
    )
}

fn notifications_card(host: SettingsHost) -> impl IntoElement {
    let master = host.notifications;
    group_card(
        icon(include_str!("../../assets/icons/activity.svg")),
        copy::text("Notifications", "通知", "通知"),
        view()
            .w_full()
            .flex_col()
            .child(toggle_row(
                copy::text("Enable notifications", "启用通知", "啟用通知"),
                copy::text(
                    "Allow Vibex to alert you outside the app",
                    "允许 Vibex 在应用外提醒你",
                    "允許 Vibex 在應用程式外提醒你",
                ),
                master,
                host.on_notifications.clone(),
                None,
            ))
            .child(divider())
            .child(toggle_row(
                copy::text("Agent completed", "Agent 已完成", "Agent 已完成"),
                copy::text(
                    "A turn finished while you were away",
                    "你离开时回合已完成",
                    "你離開時回合已完成",
                ),
                host.notify_completed,
                host.on_notify_completed.clone(),
                Some(master),
            ))
            .child(divider())
            .child(toggle_row(
                copy::text("Needs input", "需要输入", "需要輸入"),
                copy::text(
                    "An approval or an answer is waiting",
                    "有审批或回答在等待",
                    "有核准或回答在等待",
                ),
                host.notify_needs_input,
                host.on_notify_needs_input.clone(),
                Some(master),
            ))
            .child(divider())
            .child(toggle_row(
                copy::text("Failed", "执行失败", "執行失敗"),
                copy::text(
                    "A turn stopped with an error",
                    "回合因错误停止",
                    "回合因錯誤停止",
                ),
                host.notify_failed,
                host.on_notify_failed.clone(),
                Some(master),
            )),
    )
}

fn device_card(host: SettingsHost) -> impl IntoElement {
    group_card(
        icon(include_str!("../../assets/icons/settings.svg")),
        copy::text("This device", "本机", "本機"),
        view()
            .w_full()
            .flex_col()
            .child(toggle_row(
                copy::text("Haptic feedback", "触感反馈", "觸感回饋"),
                copy::text(
                    "Vibrate for gestures and finished actions",
                    "手势与操作完成时振动",
                    "手勢與操作完成時震動",
                ),
                host.haptics,
                host.on_haptics.clone(),
                None,
            ))
            .child(divider())
            .child(toggle_row(
                copy::text("Keep the screen on", "保持屏幕常亮", "保持螢幕常亮"),
                copy::text(
                    "While a turn runs or the terminal is open",
                    "当回合运行中或终端打开时",
                    "當回合執行中或終端開啟時",
                ),
                host.keep_screen_on,
                host.on_keep_screen_on.clone(),
                None,
            )),
    )
}

fn about_card(
    host: SettingsHost,
    update_status: Signal<Option<SharedString>>,
    update_url: Signal<Option<String>>,
) -> impl IntoElement {
    let on_check = {
        let status = update_status;
        let url = update_url;
        move || check_for_updates(status, url)
    };
    let on_install = host.on_install_update.clone();
    group_card(
        icon(include_str!("../../assets/icons/vibex-mark.svg")),
        copy::text("About", "关于", "關於"),
        view()
            .w_full()
            .flex_col()
            .child(
                row_shell()
                    .child(
                        view()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex_col()
                            .child(
                                text("Vibex")
                                    .text_size(px(theme::font_body()))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::foreground),
                            )
                            .child(
                                text(copy::text(
                                    "Local-first AI coding workbench",
                                    "本地优先的 AI 编程工作台",
                                    "本地優先的 AI 編程工作台",
                                ))
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::text_muted),
                            ),
                    )
                    .child(value_text(|| format!("v{}", env!("CARGO_PKG_VERSION")))),
            )
            .child(divider())
            .child(value_row(
                copy::text("Platform", "平台", "平台"),
                || {
                    if cfg!(target_os = "android") {
                        "Android".to_owned()
                    } else if cfg!(target_os = "ios") {
                        "iOS".to_owned()
                    } else {
                        std::env::consts::OS.to_owned()
                    }
                },
            ))
            .child(divider())
            .child(value_row(copy::text("License", "许可", "授權"), || {
                "AGPL-3.0-or-later".to_owned()
            }))
            .child(divider())
            .child(nav_row(
                copy::text("Check for updates", "检查更新", "檢查更新"),
                icon(include_str!("../../assets/icons/refresh.svg")),
                move || on_check(),
                || String::new(),
            ))
            .child(when(
                move || update_status.get().is_some(),
                move || {
                    text(move || update_status.get().unwrap_or_default())
                        .w_full()
                        .px(px(theme::SPACING_LG))
                        .pb(px(theme::SPACING_MD))
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_secondary)
                },
            ))
            .child(when(move || update_url.get().is_some(), {
                let on_install = on_install.clone();
                move || {
                    let on_install = on_install.clone();
                    view()
                        .w_full()
                        .px(px(theme::SPACING_LG))
                        .pb(px(theme::SPACING_MD))
                        .child(
                            action::primary(
                                copy::text("Install update", "安装更新", "安裝更新"),
                                icon(include_str!("../../assets/icons/check.svg")),
                                move || {
                                    if let Some(url) = update_url.get_untracked() {
                                        on_install(&url);
                                    }
                                },
                            )
                            .w_full(),
                        )
                }
            }))
            .child(divider())
            .child(nav_row(
                copy::text("Release notes", "发布说明", "發佈說明"),
                icon(include_str!("../../assets/icons/history.svg")),
                || open_url("https://github.com/peatboy/vibex/releases"),
                || String::new(),
            ))
            .child(divider())
            .child(nav_row(
                copy::text("Source code", "源代码", "原始碼"),
                icon(include_str!("../../assets/icons/file-text.svg")),
                || open_url("https://github.com/peatboy/vibex"),
                || String::new(),
            )),
    )
}

fn danger_card(host: SettingsHost) -> impl IntoElement {
    let on_clear = host.on_clear_runtimes.clone();
    view()
        .w_full()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_card)
        .overflow_hidden()
        .child(
            button()
                .w_full()
                .min_h(px(56.0))
                .px(px(theme::SPACING_LG))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_MD))
                .active(|style| style.opacity(0.75))
                .on_press(move || on_clear())
                .child(
                    svg(icon(include_str!("../../assets/icons/trash-2.svg")))
                        .w(px(theme::ICON_SM))
                        .h(px(theme::ICON_SM))
                        .text_color(theme::accent_red),
                )
                .child(
                    text(copy::text(
                        "Remove saved runtimes",
                        "移除已保存的运行时",
                        "移除已儲存的執行環境",
                    ))
                    .text_size(px(theme::font_body()))
                    .text_color(theme::accent_red),
                ),
        )
}

fn group_card(icon: ImageSrc, title: &'static str, body: impl IntoElement) -> impl IntoElement {
    view()
        .w_full()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_card)
        .overflow_hidden()
        .child(
            view()
                .w_full()
                .px(px(theme::SPACING_LG))
                .py(px(theme::SPACING_MD))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(icon_tile(icon, false))
                .child(
                    text(title)
                        .text_size(px(theme::font_detail()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                ),
        )
        .child(divider())
        .child(body)
}

fn row_shell() -> View {
    view()
        .w_full()
        .min_h(px(56.0))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_SM))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_MD))
}

fn label_block(label: &'static str, detail: &'static str) -> impl IntoElement {
    view()
        .flex_1()
        .min_w(px(0.0))
        .flex_col()
        .child(
            text(label)
                .text_size(px(theme::font_body()))
                .text_color(theme::foreground),
        )
        .child(
            text(detail)
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted),
        )
}

fn value_text(value: impl Fn() -> String + 'static) -> impl IntoElement {
    text(value)
        .flex_shrink_0()
        .text_size(px(theme::font_detail()))
        .text_color(theme::text_muted)
}

fn icon_tile(icon: ImageSrc, danger: bool) -> impl IntoElement {
    view()
        .w(px(30.0))
        .h(px(30.0))
        .flex_shrink_0()
        .rounded(px(8.0))
        .bg(theme::background_dim)
        .items_center()
        .justify_center()
        .child(
            svg(icon)
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .text_color(if danger {
                    theme::accent_red
                } else {
                    theme::text_secondary
                }),
        )
}

fn divider() -> impl IntoElement {
    view().w_full().h(px(1.0)).bg(theme::border_subtle)
}

fn appearance_segments(
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
        .w_full()
        .flex_row()
        .rounded(px(10.0))
        .bg(theme::background_dim)
        .p(px(2.0))
        .children(choices.into_iter().map({
            let on_appearance = on_appearance.clone();
            move |(choice, label)| {
                let on_appearance = on_appearance.clone();
                button()
                    .flex_1()
                    .h(px(36.0))
                    .rounded(px(8.0))
                    .justify_center()
                    .border_1()
                    .border_color(move || {
                        if appearance.with(|current| *current == choice) {
                            theme::border_default()
                        } else {
                            theme::background_dim().opacity(0.0)
                        }
                    })
                    .bg(move || {
                        if appearance.with(|current| *current == choice) {
                            theme::background_card()
                        } else {
                            theme::background_dim().opacity(0.0)
                        }
                    })
                    .active(|style| style.opacity(0.75))
                    .on_press(move || on_appearance(choice))
                    .child(
                        text(label)
                            .text_size(px(theme::font_detail()))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_secondary),
                    )
            }
        }))
}

fn text_size_stepper(percent: Signal<u32>, on_font_scale: Rc<dyn Fn(u32)>) -> impl IntoElement {
    let decrease = {
        let on_font_scale = on_font_scale.clone();
        move || on_font_scale(previous_font_scale_percent(percent.get_untracked()))
    };
    let increase = {
        let on_font_scale = on_font_scale.clone();
        move || on_font_scale(theme::next_font_scale_percent(percent.get_untracked()))
    };
    view()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .child(
            text(move || font_scale_label(percent.get()).to_owned())
                .text_size(px(theme::font_caption()))
                .text_color(theme::text_muted),
        )
        .child(step_button(
            "A−",
            copy::text("Decrease text size", "减小文字", "縮小文字"),
            decrease,
        ))
        .child(step_button(
            "A+",
            copy::text("Increase text size", "增大文字", "放大文字"),
            increase,
        ))
}

fn step_button(
    label: &'static str,
    accessibility: &'static str,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .w(px(36.0))
        .h(px(36.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme::border_default)
        .bg(theme::background_dim)
        .items_center()
        .justify_center()
        .active(|style| style.opacity(0.7))
        .accessibility_label(accessibility)
        .on_press(on_press)
        .child(
            text(label)
                .text_size(px(theme::font_body()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::foreground),
        )
}

fn toggle_row(
    label: &'static str,
    detail: &'static str,
    enabled: Signal<bool>,
    on_toggle: Rc<dyn Fn(bool)>,
    allowed: Option<Signal<bool>>,
) -> impl IntoElement {
    button()
        .w_full()
        .min_h(px(60.0))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_SM))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_MD))
        .opacity(move || {
            if allowed.is_some_and(|allowed| !allowed.get()) {
                0.45_f32
            } else {
                1.0_f32
            }
        })
        .active(|style| style.opacity(0.8))
        .on_press(move || {
            if allowed.is_some_and(|allowed| !allowed.get_untracked()) {
                return;
            }
            on_toggle(!enabled.get_untracked());
        })
        .child(label_block(label, detail))
        .child(toggle_track(enabled))
}

fn toggle_track(enabled: Signal<bool>) -> impl IntoElement {
    view()
        .w(px(46.0))
        .h(px(28.0))
        .flex_shrink_0()
        .rounded(px(14.0))
        .p(px(3.0))
        .flex_row()
        .items_center()
        .bg(move || {
            if enabled.get() {
                theme::foreground()
            } else {
                theme::background_dim()
            }
        })
        .child(
            view()
                .w(px(22.0))
                .h(px(22.0))
                .rounded(px(11.0))
                .bg(move || {
                    if enabled.get() {
                        theme::background()
                    } else {
                        theme::text_muted()
                    }
                })
                .ml(move || px(if enabled.get() { 18.0 } else { 0.0 })),
        )
}

fn nav_row(
    label: &'static str,
    leading: ImageSrc,
    on_press: impl Fn() + 'static,
    value: impl Fn() -> String + 'static,
) -> impl IntoElement {
    button()
        .w_full()
        .min_h(px(56.0))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_SM))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_MD))
        .active(|style| style.opacity(0.75))
        .on_press(on_press)
        .child(
            svg(leading)
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(theme::text_secondary),
        )
        .child(
            text(label)
                .flex_1()
                .text_size(px(theme::font_body()))
                .text_color(theme::foreground),
        )
        .child(value_text(value))
        .child(
            svg(icon(include_str!("../../assets/icons/chevron-right.svg")))
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(theme::text_muted),
        )
}

fn value_row(label: &'static str, value: impl Fn() -> String + 'static) -> impl IntoElement {
    row_shell()
        .child(
            text(label)
                .flex_1()
                .text_size(px(theme::font_body()))
                .text_color(theme::foreground),
        )
        .child(value_text(value))
}

fn compact_action(
    label: &'static str,
    icon: ImageSrc,
    danger: bool,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    let color = if danger {
        theme::accent_red()
    } else {
        theme::text_secondary()
    };
    button()
        .flex_1()
        .h(px(40.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(if danger {
            theme::accent_red().opacity(0.45)
        } else {
            theme::border_subtle()
        })
        .bg(theme::background_dim)
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACING_XS))
        .active(|style| style.opacity(0.75))
        .on_press(on_press)
        .child(
            svg(icon)
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(color),
        )
        .child(
            text(label)
                .truncate()
                .text_size(px(theme::font_caption()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(color),
        )
}

fn appearance_label(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::System => copy::text("System", "跟随系统", "跟隨系統"),
        Appearance::Light => copy::text("Light", "浅色", "淺色"),
        Appearance::Dark => copy::text("Dark", "深色", "深色"),
    }
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

fn previous_font_scale_percent(current: u32) -> u32 {
    let steps = theme::FONT_SCALE_STEPS;
    match steps.iter().rposition(|step| *step == current) {
        Some(0) | None => steps[steps.len() - 1],
        Some(index) => steps[index - 1],
    }
}

fn check_for_updates(status: Signal<Option<SharedString>>, url: Signal<Option<String>>) {
    url.set(None);
    status.set(Some(copy::text("Checking…", "检查中…", "檢查中…").into()));
    spawn(async move {
        let result =
            nsh_core::element::fetch("https://api.github.com/repos/peatboy/vibex/releases/latest")
                .await;
        let message = match result {
            Ok(response) if response.ok() => {
                let body = response.text();
                let tag = body
                    .split("\"tag_name\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').nth(1))
                    .map(str::to_owned);
                let download = body
                    .match_indices("\"browser_download_url\"")
                    .filter_map(|(index, _)| {
                        let rest = &body[index + 22..];
                        let value = rest.split('"').nth(1)?;
                        value.ends_with(".apk").then(|| value.to_owned())
                    })
                    .next();
                url.set(download);
                match tag {
                    Some(tag) => {
                        let tag = tag.trim_start_matches('v');
                        if tag == env!("CARGO_PKG_VERSION") {
                            copy::text("Up to date", "已是最新版本", "已是最新版本").to_owned()
                        } else {
                            format!(
                                "{} {tag}",
                                copy::text("Available:", "可更新到：", "可更新至：")
                            )
                        }
                    }
                    None => copy::text("No release information", "没有发布信息", "沒有發佈資訊")
                        .to_owned(),
                }
            }
            Ok(_) => copy::text("Update check failed", "检查更新失败", "檢查更新失敗").to_owned(),
            Err(error) => format!(
                "{}: {error}",
                copy::text("Update check failed", "检查更新失败", "檢查更新失敗")
            ),
        };
        status.set(Some(message.into()));
    });
}

fn open_url(url: &'static str) {
    spawn(async move {
        let _ = nsh_core::element::open_url(url).await;
    });
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

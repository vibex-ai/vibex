//! The settings centre: a root index and a page for each topic.
//!
//! The root names each topic and what it is set to; a tap opens that topic's
//! own page. The value-bearing controls stay on the page they belong to, so a
//! reader sees one decision at a time instead of a long column of switches.
//! Back, the header's chevron and the Android back key all walk the same
//! stack, one page at a time, before the centre itself closes.

use std::rc::Rc;

use nsh_base::button;
use nsh_core::{element::ImageSrc, platform::Appearance, prelude::*};

use crate::{copy, theme, ui::action};

/// The page the settings centre is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPage {
    /// The index: one row per topic.
    #[default]
    Root,
    Runtime,
    Appearance,
    Language,
    Conversation,
    Notifications,
    Device,
    About,
}

impl SettingsPage {
    fn title(self) -> &'static str {
        match self {
            SettingsPage::Root => copy::text("Settings", "设置", "設定"),
            SettingsPage::Runtime => copy::text("Runtime", "运行时", "執行環境"),
            SettingsPage::Appearance => copy::text("Appearance", "外观", "外觀"),
            SettingsPage::Language => copy::text("Language", "语言", "語言"),
            SettingsPage::Conversation => copy::text("Conversation", "会话", "工作階段"),
            SettingsPage::Notifications => copy::text("Notifications", "通知", "通知"),
            SettingsPage::Device => copy::text("This device", "本机", "本機"),
            SettingsPage::About => copy::text("About", "关于", "關於"),
        }
    }
}

/// The page's outer gutter.
const PAGE_GUTTER: f32 = 16.0;
/// The gap between two cards.
const CARD_GAP: f32 = 0.0;
/// The corner radius of a card.
const CARD_RADIUS: f32 = 16.0;
/// The height of the title bar.
const HEADER_HEIGHT: f32 = 56.0;
/// The minimum height of a settings row.
const ROW_MIN: f32 = 60.0;
/// The leading icon tile and its glyph.
const TILE: f32 = 30.0;
const TILE_ICON: f32 = 20.0;

/// What the settings centre may do, and the state it shows.
#[derive(Clone)]
pub struct SettingsHost {
    pub open: Signal<bool>,
    /// The page currently shown.
    pub page: Signal<SettingsPage>,
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
    /// The runtimes this phone saved.
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

/// The settings centre, presented over whatever opened it.
pub fn settings(host: SettingsHost) -> impl IntoElement {
    let open = host.open;
    let page = host.page;
    dynamic(move || {
        if open.get() {
            let page = page.get();
            page_shell(host.clone(), page).into_element()
        } else {
            view().into_element()
        }
    })
}

fn page_shell(host: SettingsHost, page: SettingsPage) -> impl IntoElement {
    let confirm_clear = signal(false);
    view()
        .absolute()
        .inset(px(0.0))
        .z_index(30)
        .bg(theme::block_page)
        .flex_col()
        .child(header(host.clone(), page))
        .child(
            view().flex_1().flex_col().child(
                scroll().flex_1().child(
                    view()
                        .w_full()
                        .flex_col()
                        .gap(px(CARD_GAP))
                        .px(px(PAGE_GUTTER))
                        .pt(px(theme::SPACING_SM))
                        .pb_safe_area_plus(px(theme::SPACING_XL))
                        .child(body(host.clone(), page, confirm_clear)),
                ),
            ),
        )
        .child(clear_dialog(host, confirm_clear))
}

fn body(host: SettingsHost, page: SettingsPage, confirm_clear: Signal<bool>) -> AnyElement {
    match page {
        SettingsPage::Root => root_page(host, confirm_clear).into_element(),
        SettingsPage::Runtime => runtime_page(host).into_element(),
        SettingsPage::Appearance => appearance_page(host).into_element(),
        SettingsPage::Language => language_page(host).into_element(),
        SettingsPage::Conversation => conversation_page(host).into_element(),
        SettingsPage::Notifications => notifications_page(host).into_element(),
        SettingsPage::Device => device_page(host).into_element(),
        SettingsPage::About => about_page(host).into_element(),
    }
}

fn header(host: SettingsHost, page: SettingsPage) -> impl IntoElement {
    let on_back: Rc<dyn Fn()> = if page == SettingsPage::Root {
        host.on_close.clone()
    } else {
        let page = host.page;
        Rc::new(move || page.set(SettingsPage::Root))
    };
    view().pt_safe_area().child(
        view()
            .h(px(HEADER_HEIGHT))
            .flex_shrink_0()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .px(px(8.0))
            .child(
                button()
                    .size(px(40.0))
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .rounded(px(20.0))
                    .active(|style| style.opacity(0.6))
                    .accessibility_label(copy::text("Back", "返回", "返回"))
                    .on_press(move || on_back())
                    .child(
                        svg(icon(include_str!("../../assets/icons/chevron-left.svg")))
                            .w(px(22.0))
                            .h(px(22.0))
                            .text_color(theme::foreground),
                    ),
            )
            .child(
                view().flex_1().items_center().child(
                    text(page.title())
                        .truncate()
                        .text_size(px(theme::font_body()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                ),
            )
            .child(view().w(px(40.0)).h(px(40.0))),
    )
}

/// The index: what each topic is set to, and the way into it.
fn root_page(host: SettingsHost, confirm_clear: Signal<bool>) -> impl IntoElement {
    let appearance = host.appearance;
    let language = host.language;
    let notifications = host.notifications;
    let saved = host.saved;
    let mut app_rows: Vec<AnyElement> = Vec::new();
    app_rows.push(
        index_row(
            icon(include_str!("../../assets/icons/palette.svg")),
            copy::text("Appearance", "外观", "外觀"),
            move || appearance_label(appearance.get()).to_owned(),
            navigate(host.page, SettingsPage::Appearance),
        )
        .into_element(),
    );
    app_rows.push(divider_from(60.0).into_element());
    app_rows.push(
        index_row(
            icon(include_str!("../../assets/icons/globe.svg")),
            copy::text("Language", "语言", "語言"),
            move || language_label(language.get()).to_owned(),
            navigate(host.page, SettingsPage::Language),
        )
        .into_element(),
    );
    app_rows.push(divider_from(60.0).into_element());
    app_rows.push(
        index_row(
            icon(include_str!("../../assets/icons/message-square.svg")),
            copy::text("Conversation", "会话", "工作階段"),
            || String::new(),
            navigate(host.page, SettingsPage::Conversation),
        )
        .into_element(),
    );
    app_rows.push(divider_from(60.0).into_element());
    app_rows.push(
        index_row(
            icon(include_str!("../../assets/icons/activity.svg")),
            copy::text("Notifications", "通知", "通知"),
            move || {
                if notifications.get() {
                    copy::text("On", "开", "開").to_owned()
                } else {
                    copy::text("Off", "关", "關").to_owned()
                }
            },
            navigate(host.page, SettingsPage::Notifications),
        )
        .into_element(),
    );
    let device_row = index_row(
        icon(include_str!("../../assets/icons/settings.svg")),
        copy::text("This device", "本机", "本機"),
        || String::new(),
        navigate(host.page, SettingsPage::Device),
    );
    let about_row = index_row(
        icon(include_str!("../../assets/icons/vibex-mark.svg")),
        copy::text("About Vibex", "关于 Vibex", "關於 Vibex"),
        || format!("v{}", env!("CARGO_PKG_VERSION")),
        navigate(host.page, SettingsPage::About),
    );
    let danger = when(
        move || !saved.with(|saved| saved.is_empty()),
        move || danger_card(confirm_clear),
    );
    view()
        .w_full()
        .flex_col()
        .gap(px(CARD_GAP))
        .child(runtime_summary(host.clone()))
        .child(section_label(copy::text("App", "应用", "應用")))
        .child(plain_card(app_rows))
        .child(section_label(copy::text("Device", "设备", "裝置")))
        .child(plain_card(vec![device_row.into_element()]))
        .child(section_label(copy::text("More", "更多", "更多")))
        .child(plain_card(vec![about_row.into_element()]))
        .child(danger)
        .child(
            text(move || {
                copy::text(
                    "Settings apply to this phone only.",
                    "以上设置仅作用于这台手机。",
                    "以上設定僅作用於這台手機。",
                )
            })
            .w_full()
            .px(px(4.0))
            .pt(px(4.0))
            .text_size(px(theme::font_hint()))
            .text_color(subtitle_text),
        )
}

fn runtime_summary(host: SettingsHost) -> impl IntoElement {
    let runtime = host.runtime;
    let saved = host.saved;
    let page = host.page;
    let connected = move || runtime.get().is_some();
    button()
        .w_full()
        .rounded(px(CARD_RADIUS))
        .shadow([card_shadow()])
        .mb(px(12.0))
        .bg(theme::block_surface)
        .px(px(theme::SPACING_LG))
        .py(px(14.0))
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .active(|style| style.opacity(0.85))
        .on_press(move || page.set(SettingsPage::Runtime))
        .child(
            view()
                .size(px(36.0))
                .flex_shrink_0()
                .rounded(px(18.0))
                .bg(move || {
                    if connected() {
                        theme::color_token("chart-1").opacity(0.16)
                    } else {
                        theme::block_inset()
                    }
                })
                .items_center()
                .justify_center()
                .child(
                    svg(icon(include_str!("../../assets/icons/monitor.svg")))
                        .w(px(20.0))
                        .h(px(20.0))
                        .text_color(move || {
                            if connected() {
                                theme::color_token("chart-1")
                            } else {
                                theme::text_secondary()
                            }
                        }),
                ),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .flex_col()
                .child(
                    text(move || {
                        runtime
                            .get()
                            .map(|label| label.to_string())
                            .unwrap_or_else(|| {
                                copy::text(
                                    "No runtime connected",
                                    "未连接运行时",
                                    "尚未連線執行環境",
                                )
                                .to_owned()
                            })
                    })
                    .truncate()
                    .text_size(px(theme::font_detail()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::foreground),
                )
                .child(
                    text(move || {
                        let count = saved.with(|saved| saved.len());
                        let status = if connected() {
                            copy::text("Connected", "已连接", "已連線")
                        } else {
                            copy::text("Not connected", "未连接", "未連線")
                        };
                        if count == 0 {
                            format!(
                                "{status} · {}",
                                copy::text(
                                    "no saved runtimes",
                                    "没有已保存的运行时",
                                    "沒有已儲存的執行環境"
                                )
                            )
                        } else {
                            format!(
                                "{status} · {count} {}",
                                copy::text("saved", "个已保存", "個已儲存")
                            )
                        }
                    })
                    .text_size(px(theme::font_micro()))
                    .text_color(subtitle_text),
                ),
        )
        .child(
            svg(icon(include_str!("../../assets/icons/chevron-right.svg")))
                .w(px(theme::ICON_MD))
                .h(px(theme::ICON_MD))
                .flex_shrink_0()
                .text_color(subtle_icon),
        )
}

fn runtime_page(host: SettingsHost) -> impl IntoElement {
    let connected = move || host.runtime.get().is_some();
    let actions = {
        let on_reconnect = host.on_reconnect.clone();
        let on_switch = host.on_switch_runtime.clone();
        let on_disconnect = host.on_disconnect.clone();
        move || {
            view()
                .w_full()
                .flex_col()
                .child(action_row(
                    icon(include_str!("../../assets/icons/refresh.svg")),
                    copy::text("Reconnect", "重新连接", "重新連線"),
                    copy::text(
                        "Retry this runtime without pairing again",
                        "无需重新配对即可重试",
                        "無需重新配對即可重試",
                    ),
                    false,
                    false,
                    {
                        let on_reconnect = on_reconnect.clone();
                        move || on_reconnect()
                    },
                ))
                .child(divider_from(60.0))
                .child(action_row(
                    icon(include_str!("../../assets/icons/chevron-right.svg")),
                    copy::text("Switch runtime", "切换运行时", "切換執行環境"),
                    copy::text(
                        "Choose, rename or remove a saved runtime",
                        "选择、重命名或移除已保存的运行时",
                        "選擇、重新命名或移除已儲存的執行環境",
                    ),
                    false,
                    true,
                    {
                        let on_switch = on_switch.clone();
                        move || on_switch()
                    },
                ))
                .child(divider_from(60.0))
                .child(action_row(
                    icon(include_str!("../../assets/icons/plug-zap.svg")),
                    copy::text("Disconnect", "断开连接", "中斷連線"),
                    copy::text(
                        "End this connection; nothing is unpaired",
                        "仅结束本次连接，不会解除配对",
                        "僅結束本次連線，不會解除配對",
                    ),
                    true,
                    false,
                    {
                        let on_disconnect = on_disconnect.clone();
                        move || on_disconnect()
                    },
                ))
        }
    };
    view()
        .w_full()
        .flex_col()
        .gap(px(CARD_GAP))
        .child(runtime_summary(host.clone()))
        .child(when(connected, {
            let actions = actions;
            move || {
                view()
                    .w_full()
                    .flex_col()
                    .gap(px(CARD_GAP))
                    .child(section_label(copy::text("Connection", "连接", "連線")))
                    .child(plain_card(vec![actions().into_element()]))
            }
        }))
        .child(when(move || !connected(), {
            let on_close = host.on_close.clone();
            move || {
                let on_close = on_close.clone();
                view()
                    .w_full()
                    .flex_col()
                    .gap(px(CARD_GAP))
                    .child(section_label(copy::text("Pairing", "配对", "配對")))
                    .child(footer_note(copy::text(
                        "Open pairing to scan a code, find a runtime nearby, or paste a pairing link.",
                        "打开配对页扫码、查找附近的运行时，或粘贴配对链接。",
                        "開啟配對頁掃碼、尋找附近的執行環境，或貼上配對連結。",
                    )))
                    .child(
                        action::primary(
                            copy::text("Open pairing", "打开配对", "開啟配對"),
                            icon(include_str!("../../assets/icons/chevron-left.svg")),
                            move || on_close(),
                        )
                        .w_full(),
                    )
            }
        }))
}

fn appearance_page(host: SettingsHost) -> impl IntoElement {
    let appearance = host.appearance;
    let font_scale = host.font_scale;
    let choices = [
        (
            Appearance::System,
            copy::text("System", "跟随系统", "跟隨系統"),
        ),
        (Appearance::Light, copy::text("Light", "浅色", "淺色")),
        (Appearance::Dark, copy::text("Dark", "深色", "深色")),
    ];
    let mut theme_rows: Vec<AnyElement> = Vec::new();
    for (index, (choice, label)) in choices.into_iter().enumerate() {
        if index > 0 {
            theme_rows.push(divider_from(50.0).into_element());
        }
        theme_rows.push(
            radio_row(label, None, appearance, choice, {
                let on_appearance = host.on_appearance.clone();
                move || on_appearance(choice)
            })
            .into_element(),
        );
    }
    view()
        .w_full()
        .flex_col()
        .gap(px(CARD_GAP))
        .child(section_label(copy::text("Theme", "主题", "主題")))
        .child(plain_card(theme_rows))
        .child(section_label(copy::text(
            "Text size",
            "文字大小",
            "文字大小",
        )))
        .child(plain_card(vec![
            row_shell()
                .child(label_block(
                    copy::text("Scale", "缩放比例", "縮放比例"),
                    copy::text(
                        "Scales copy and spacing across the app",
                        "缩放应用内的文字与间距",
                        "縮放應用程式內的文字與間距",
                    ),
                ))
                .child(text_size_stepper(font_scale, host.on_font_scale.clone()))
                .into_element(),
            divider().into_element(),
            view()
                .w_full()
                .px(px(16.0))
                .py(px(12.0))
                .child(
                    view()
                        .w_full()
                        .rounded(px(10.0))
                        .bg(theme::block_inset)
                        .p(px(12.0))
                        .child(
                            text(move || format!("Aa  {}", font_scale_label(font_scale.get())))
                                .text_size(move || {
                                    px(font_scale.get() as f32 / 100.0 * theme::FONT_BODY)
                                })
                                .text_color(theme::text_secondary),
                        ),
                )
                .into_element(),
        ]))
        .child(section_label(copy::text("Motion", "动效", "動效")))
        .child(plain_card(vec![
            toggle_row(
                copy::text("Reduce motion", "减少动效", "減少動效"),
                copy::text(
                    "Skip page and sheet transitions",
                    "跳过页面与弹层的过渡动画",
                    "跳過頁面與彈層的轉場動畫",
                ),
                host.reduce_motion,
                host.on_reduce_motion.clone(),
                None,
            )
            .into_element(),
        ]))
}

fn language_page(host: SettingsHost) -> impl IntoElement {
    use copy::LanguagePreference as Choice;
    let language = host.language;
    let choices = [
        (
            Choice::System,
            copy::text("System", "跟随系统", "跟隨系統"),
            Some(copy::text(
                "Use the phone's language",
                "使用手机的系统语言",
                "使用手機的系統語言",
            )),
        ),
        (Choice::English, "English", None),
        (Choice::SimplifiedChinese, "简体中文", None),
        (Choice::TraditionalChinese, "繁體中文", None),
    ];
    let mut rows: Vec<AnyElement> = Vec::new();
    for (index, (choice, label, detail)) in choices.into_iter().enumerate() {
        if index > 0 {
            rows.push(divider_from(50.0).into_element());
        }
        rows.push(
            radio_row(label, detail, language, choice, {
                let on_language = host.on_language.clone();
                move || on_language(choice)
            })
            .into_element(),
        );
    }
    view()
        .w_full()
        .flex_col()
        .gap(px(CARD_GAP))
        .child(section_label(copy::text(
            "Interface language",
            "界面语言",
            "介面語言",
        )))
        .child(plain_card(rows))
        .child(
            text(copy::text(
                "The choice applies at once and is kept for the next launch.",
                "更改会立即生效，并在下次启动时保留。",
                "變更會立即生效，並在下次啟動時保留。",
            ))
            .w_full()
            .px(px(16.0))
            .text_size(px(theme::font_hint()))
            .text_color(subtitle_text),
        )
}

fn conversation_page(host: SettingsHost) -> impl IntoElement {
    let mut rows: Vec<AnyElement> = Vec::new();
    rows.push(
        toggle_row(
            copy::text("Token usage", "Token 使用量", "Token 使用量"),
            copy::text(
                "Show the session's token counters above the composer",
                "在输入框上方显示本次会话的 Token 用量",
                "在輸入框上方顯示本次工作階段的 Token 用量",
            ),
            host.show_token_usage,
            host.on_show_token_usage.clone(),
            None,
        )
        .into_element(),
    );
    rows.push(divider().into_element());
    rows.push(
        toggle_row(
            copy::text("Plan and goal", "计划与目标", "計畫與目標"),
            copy::text(
                "Show the plan and goal bars the runtime publishes",
                "显示运行时提供的计划与目标",
                "顯示執行環境提供的計畫與目標",
            ),
            host.show_plan_goal,
            host.on_show_plan_goal.clone(),
            None,
        )
        .into_element(),
    );
    rows.push(divider().into_element());
    rows.push(
        toggle_row(
            copy::text("Generation status", "生成状态", "生成狀態"),
            copy::text(
                "Show a live line while the Agent works",
                "Agent 工作时显示实时状态行",
                "Agent 工作時顯示即時狀態列",
            ),
            host.show_generation_status,
            host.on_show_generation_status.clone(),
            None,
        )
        .into_element(),
    );
    rows.push(divider().into_element());
    rows.push(
        toggle_row(
            copy::text("Reasoning", "思考过程", "思考過程"),
            copy::text(
                "Show the Agent's reasoning rows",
                "显示 Agent 的推理内容",
                "顯示 Agent 的推理內容",
            ),
            host.show_reasoning,
            host.on_show_reasoning.clone(),
            None,
        )
        .into_element(),
    );
    rows.push(divider().into_element());
    rows.push(
        toggle_row(
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
        )
        .into_element(),
    );
    view()
        .w_full()
        .flex_col()
        .gap(px(CARD_GAP))
        .child(section_label(copy::text("Timeline", "时间线", "時間線")))
        .child(plain_card(rows))
        .child(footer_note(copy::text(
            "These choices change what the transcript draws on this phone.",
            "这些选项只影响这台手机上时间线的显示内容。",
            "這些選項只影響這台手機上時間線的顯示內容。",
        )))
}

fn notifications_page(host: SettingsHost) -> impl IntoElement {
    let master = host.notifications;
    let mut rows: Vec<AnyElement> = Vec::new();
    rows.push(
        toggle_row(
            copy::text("Enable notifications", "启用通知", "啟用通知"),
            copy::text(
                "Allow Vibex to alert you outside the app",
                "允许 Vibex 在应用外提醒你",
                "允許 Vibex 在應用程式外提醒你",
            ),
            master,
            host.on_notifications.clone(),
            None,
        )
        .into_element(),
    );
    rows.push(divider().into_element());
    rows.push(
        toggle_row(
            copy::text("Agent completed", "Agent 已完成", "Agent 已完成"),
            copy::text(
                "A turn finished while you were away",
                "你离开时回合已完成",
                "你離開時回合已完成",
            ),
            host.notify_completed,
            host.on_notify_completed.clone(),
            Some(master),
        )
        .into_element(),
    );
    rows.push(divider().into_element());
    rows.push(
        toggle_row(
            copy::text("Needs input", "需要输入", "需要輸入"),
            copy::text(
                "An approval or an answer is waiting",
                "有审批或回答在等待",
                "有核准或回答在等待",
            ),
            host.notify_needs_input,
            host.on_notify_needs_input.clone(),
            Some(master),
        )
        .into_element(),
    );
    rows.push(divider().into_element());
    rows.push(
        toggle_row(
            copy::text("Failed", "执行失败", "執行失敗"),
            copy::text(
                "A turn stopped with an error",
                "回合因错误停止",
                "回合因錯誤停止",
            ),
            host.notify_failed,
            host.on_notify_failed.clone(),
            Some(master),
        )
        .into_element(),
    );
    view()
        .w_full()
        .flex_col()
        .gap(px(CARD_GAP))
        .child(section_label(copy::text("Alerts", "提醒", "提醒")))
        .child(plain_card(rows))
        .child(footer_note(copy::text(
            "Category switches stay off while notifications are disabled.",
            "关闭通知后，分类开关会保持不可操作。",
            "關閉通知後，分類開關會保持不可操作。",
        )))
}

fn device_page(host: SettingsHost) -> impl IntoElement {
    let mut rows: Vec<AnyElement> = Vec::new();
    rows.push(
        toggle_row(
            copy::text("Haptic feedback", "触感反馈", "觸感回饋"),
            copy::text(
                "Vibrate for gestures and finished actions",
                "手势与操作完成时振动",
                "手勢與操作完成時震動",
            ),
            host.haptics,
            host.on_haptics.clone(),
            None,
        )
        .into_element(),
    );
    rows.push(divider().into_element());
    rows.push(
        toggle_row(
            copy::text("Keep the screen on", "保持屏幕常亮", "保持螢幕常亮"),
            copy::text(
                "While a turn runs or the terminal is open",
                "当回合运行中或终端打开时",
                "當回合執行中或終端開啟時",
            ),
            host.keep_screen_on,
            host.on_keep_screen_on.clone(),
            None,
        )
        .into_element(),
    );
    view()
        .w_full()
        .flex_col()
        .gap(px(CARD_GAP))
        .child(section_label(copy::text("Feedback", "反馈", "回饋")))
        .child(plain_card(rows))
}

fn about_page(host: SettingsHost) -> impl IntoElement {
    let update_status = signal(None::<SharedString>);
    let update_url = signal(None::<String>);
    let on_check = {
        let status = update_status;
        let url = update_url;
        move || check_for_updates(status, url)
    };
    let on_install = host.on_install_update.clone();
    view()
        .w_full()
        .flex_col()
        .gap(px(CARD_GAP))
        .child(
            view()
                .w_full()
                .rounded(px(CARD_RADIUS))
                .shadow([card_shadow()])
                .mb(px(12.0))
                .bg(theme::block_surface)
                .px(px(16.0))
                .py(px(14.0))
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .child(
                    view()
                        .size(px(36.0))
                        .flex_shrink_0()
                        .rounded(px(18.0))
                        .bg(theme::block_inset)
                        .items_center()
                        .justify_center()
                        .child(
                            svg(icon(include_str!("../../assets/icons/vibex-mark.svg")))
                                .w(px(20.0))
                                .h(px(20.0))
                                .text_color(theme::foreground),
                        ),
                )
                .child(
                    view()
                        .flex_1()
                        .flex_col()
                        .gap(px(3.0))
                        .child(
                            text("Vibex")
                                .text_size(px(theme::font_detail()))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::foreground),
                        )
                        .child(
                            text(move || {
                                format!(
                                    "v{} · {}",
                                    env!("CARGO_PKG_VERSION"),
                                    copy::text(
                                        "Local-first AI coding workbench",
                                        "本地优先的 AI 编程工作台",
                                        "本地優先的 AI 編程工作台",
                                    )
                                )
                            })
                            .text_size(px(theme::font_micro()))
                            .text_color(subtitle_text),
                        ),
                ),
        )
        .child(section_label(copy::text("App", "应用", "應用")))
        .child(plain_card(vec![
            value_row(copy::text("Platform", "平台", "平台"), || {
                if cfg!(target_os = "android") {
                    "Android".to_owned()
                } else if cfg!(target_os = "ios") {
                    "iOS".to_owned()
                } else {
                    std::env::consts::OS.to_owned()
                }
            })
            .into_element(),
            divider().into_element(),
            value_row(copy::text("License", "许可", "授權"), || {
                "AGPL-3.0-or-later".to_owned()
            })
            .into_element(),
        ]))
        .child(section_label(copy::text("Updates", "更新", "更新")))
        .child(plain_card(vec![
            nav_row(
                copy::text("Check for updates", "检查更新", "檢查更新"),
                icon(include_str!("../../assets/icons/refresh.svg")),
                move || on_check(),
            )
            .into_element(),
            when(
                move || update_status.get().is_some(),
                move || {
                    text(move || update_status.get().unwrap_or_default())
                        .w_full()
                        .px(px(theme::SPACING_LG))
                        .pb(px(theme::SPACING_MD))
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_secondary)
                },
            )
            .into_element(),
            when(move || update_url.get().is_some(), {
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
            })
            .into_element(),
            divider().into_element(),
            nav_row(
                copy::text("Release notes", "发布说明", "發佈說明"),
                icon(include_str!("../../assets/icons/history.svg")),
                || open_url("https://github.com/peatboy/vibex/releases"),
            )
            .into_element(),
        ]))
        .child(section_label(copy::text("Source", "来源", "來源")))
        .child(plain_card(vec![
            nav_row(
                copy::text("Source code", "源代码", "原始碼"),
                icon(include_str!("../../assets/icons/file-text.svg")),
                || open_url("https://github.com/peatboy/vibex"),
            )
            .into_element(),
        ]))
}

fn danger_card(confirm_clear: Signal<bool>) -> impl IntoElement {
    view()
        .w_full()
        .rounded(px(CARD_RADIUS))
        .shadow([card_shadow()])
        .mb(px(12.0))
        .bg(theme::block_surface)
        .overflow_hidden()
        .child(
            button()
                .w_full()
                .min_h(px(ROW_MIN))
                .px(px(16.0))
                .py(px(9.0))
                .flex_row()
                .items_center()
                .gap(px(14.0))
                .active(|style| style.opacity(0.75))
                .on_press(move || confirm_clear.set(true))
                .child(icon_tile(
                    icon(include_str!("../../assets/icons/trash-2.svg")),
                    true,
                ))
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex_col()
                        .child(
                            text(copy::text(
                                "Remove saved runtimes",
                                "移除已保存的运行时",
                                "移除已儲存的執行環境",
                            ))
                            .text_size(px(theme::font_detail()))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::accent_red),
                        )
                        .child(
                            text(copy::text(
                                "Every credential leaves this phone",
                                "所有凭据都会从这台手机上删除",
                                "所有憑證都會從這台手機上刪除",
                            ))
                            .text_size(px(theme::font_micro()))
                            .text_color(subtitle_text),
                        ),
                ),
        )
}

/// The confirmation for the one destructive action on the page.
fn clear_dialog(host: SettingsHost, open: Signal<bool>) -> impl IntoElement {
    let on_clear = host.on_clear_runtimes.clone();
    view().child(when(move || open.get(), {
        let on_clear = on_clear.clone();
        move || {
            let on_clear = on_clear.clone();
            view()
                .absolute()
                .inset(px(0.0))
                .z_index(40)
                .bg(nsh_core::color::black().opacity(0.55))
                .p(px(24.0))
                .items_center()
                .justify_center()
                .on_press(|| {})
                .child(
                    view()
                        .w_full()
                        .max_w(px(360.0))
                        .rounded(px(24.0))
                        .shadow([dialog_shadow()])
                        .bg(theme::block_surface)
                        .p(px(23.0))
                        .flex_col()
                        .gap(px(12.0))
                        .child(
                            text(copy::text(
                                "Remove all saved runtimes?",
                                "移除所有已保存的运行时？",
                                "移除所有已儲存的執行環境？",
                            ))
                            .text_size(px(theme::font_heading()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                        )
                        .child(
                            text(copy::text(
                                "Every credential leaves this phone; pair again to reconnect.",
                                "所有凭据都会从这台手机上删除，需要重新配对才能连接。",
                                "所有憑證都會從這台手機上刪除，需要重新配對才能連線。",
                            ))
                            .text_size(px(theme::font_detail()))
                            .line_height(theme::line_height_for(theme::font_detail()))
                            .text_color(theme::text_secondary),
                        )
                        .child(
                            view()
                                .flex_row()
                                .gap(px(10.0))
                                .pt(px(10.0))
                                .child(
                                    button()
                                        .flex_1()
                                        .h(px(44.0))
                                        .rounded(px(14.0))
                                        .bg(theme::block_inset)
                                        .items_center()
                                        .justify_center()
                                        .active(|style| style.opacity(0.75))
                                        .on_press(move || open.set(false))
                                        .child(
                                            text(copy::text("Cancel", "取消", "取消"))
                                                .text_size(px(theme::font_detail()))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme::foreground),
                                        ),
                                )
                                .child(
                                    button()
                                        .flex_1()
                                        .h(px(44.0))
                                        .rounded(px(14.0))
                                        .bg(theme::accent_red().opacity(0.15))
                                        .items_center()
                                        .justify_center()
                                        .active(|style| style.opacity(0.75))
                                        .on_press({
                                            let on_clear = on_clear.clone();
                                            move || {
                                                on_clear();
                                                open.set(false);
                                            }
                                        })
                                        .child(
                                            text(copy::text("Remove", "移除", "移除"))
                                                .text_size(px(theme::font_detail()))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme::accent_red),
                                        ),
                                ),
                        ),
                )
        }
    }))
}

fn plain_card(rows: Vec<AnyElement>) -> impl IntoElement {
    view()
        .w_full()
        .rounded(px(CARD_RADIUS))
        .shadow([card_shadow()])
        .mb(px(12.0))
        .bg(theme::block_surface)
        .overflow_hidden()
        .child(view().w_full().flex_col().children(rows))
}

fn index_row(
    leading: ImageSrc,
    title: &'static str,
    value: impl Fn() -> String + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .w_full()
        .min_h(px(ROW_MIN))
        .px(px(16.0))
        .py(px(9.0))
        .flex_row()
        .items_center()
        .gap(px(14.0))
        .active(|style| style.opacity(0.8))
        .accessibility_label(title)
        .on_press(on_press)
        .child(icon_tile(leading, false))
        .child(
            view().flex_1().min_w(px(0.0)).child(
                text(title)
                    .text_size(px(theme::font_body()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::foreground),
            ),
        )
        .child(
            text(value)
                .flex_shrink_0()
                .text_size(px(theme::font_body()))
                .text_color(subtle_text),
        )
        .child(
            svg(icon(include_str!("../../assets/icons/chevron-right.svg")))
                .w(px(theme::ICON_MD))
                .h(px(theme::ICON_MD))
                .flex_shrink_0()
                .text_color(subtle_icon),
        )
}

fn action_row(
    leading: ImageSrc,
    title: &'static str,
    detail: &'static str,
    danger: bool,
    chevron: bool,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    let color = if danger {
        theme::accent_red()
    } else {
        theme::foreground()
    };
    button()
        .w_full()
        .min_h(px(ROW_MIN))
        .px(px(16.0))
        .py(px(9.0))
        .flex_row()
        .items_center()
        .gap(px(14.0))
        .active(|style| style.opacity(0.8))
        .on_press(on_press)
        .child(icon_tile(leading, danger))
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .flex_col()
                .gap(px(3.0))
                .child(
                    text(title)
                        .text_size(px(theme::font_body()))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color),
                )
                .child(
                    text(detail)
                        .text_size(px(theme::font_micro()))
                        .text_color(subtitle_text),
                ),
        )
        .child(when(chevron, move || {
            svg(icon(include_str!("../../assets/icons/chevron-right.svg")))
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(if danger {
                    theme::accent_red()
                } else {
                    subtle_icon()
                })
        }))
}

fn radio_row<T: Copy + PartialEq + 'static>(
    label: &'static str,
    detail: Option<&'static str>,
    selected: Signal<T>,
    choice: T,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .w_full()
        .min_h(px(ROW_MIN))
        .px(px(16.0))
        .py(px(9.0))
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .active(|style| style.opacity(0.75))
        .accessibility_label(label)
        .on_press(on_press)
        .child(radio_mark(selected, choice))
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .flex_col()
                .child(
                    text(label)
                        .text_size(px(theme::font_detail()))
                        .text_color(theme::foreground),
                )
                .child(when(
                    move || detail.is_some(),
                    move || {
                        text(detail.unwrap_or_default())
                            .text_size(px(theme::font_micro()))
                            .text_color(subtitle_text)
                    },
                )),
        )
}

fn radio_mark<T: Copy + PartialEq + 'static>(selected: Signal<T>, choice: T) -> impl IntoElement {
    view()
        .w(px(22.0))
        .h(px(22.0))
        .flex_shrink_0()
        .rounded(px(11.0))
        .border_1()
        .border_color(move || {
            if selected.with(|current| *current == choice) {
                theme::foreground()
            } else {
                theme::border_default()
            }
        })
        .bg(move || {
            if selected.with(|current| *current == choice) {
                theme::foreground()
            } else {
                theme::block_surface().opacity(0.0)
            }
        })
        .items_center()
        .justify_center()
        .child(when(
            move || selected.with(|current| *current == choice),
            || {
                view()
                    .w(px(10.0))
                    .h(px(10.0))
                    .rounded(px(5.0))
                    .bg(theme::background)
            },
        ))
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
                .text_color(subtle_text),
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
        .w(px(40.0))
        .h(px(40.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(theme::border_default)
        .bg(theme::block_inset)
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
        .min_h(px(ROW_MIN))
        .px(px(16.0))
        .py(px(9.0))
        .flex_row()
        .items_center()
        .gap(px(14.0))
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
        .w(px(44.0))
        .h(px(26.0))
        .flex_shrink_0()
        .rounded(px(13.0))
        .p(px(2.0))
        .flex_row()
        .items_center()
        .bg(move || {
            if enabled.get() {
                theme::foreground()
            } else {
                theme::block_inset()
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
                        subtle_icon()
                    }
                })
                .ml(move || px(if enabled.get() { 16.0 } else { 0.0 })),
        )
}

fn row_shell() -> View {
    view()
        .w_full()
        .min_h(px(ROW_MIN))
        .px(px(16.0))
        .py(px(9.0))
        .flex_row()
        .items_center()
        .gap(px(14.0))
}

fn label_block(label: &'static str, detail: &'static str) -> impl IntoElement {
    view()
        .flex_1()
        .min_w(px(0.0))
        .flex_col()
        .gap(px(3.0))
        .child(
            text(label)
                .text_size(px(theme::font_body()))
                .text_color(theme::foreground),
        )
        .child(
            text(detail)
                .text_size(px(theme::font_micro()))
                .text_color(subtitle_text),
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
        .child(
            text(value)
                .flex_shrink_0()
                .text_size(px(theme::font_body()))
                .text_color(subtle_text),
        )
}

fn nav_row(
    label: &'static str,
    leading: ImageSrc,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    button()
        .w_full()
        .min_h(px(ROW_MIN))
        .px(px(16.0))
        .py(px(9.0))
        .flex_row()
        .items_center()
        .gap(px(14.0))
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
        .child(
            svg(icon(include_str!("../../assets/icons/chevron-right.svg")))
                .w(px(theme::ICON_MD))
                .h(px(theme::ICON_MD))
                .flex_shrink_0()
                .text_color(subtle_icon),
        )
}

fn icon_tile(image: ImageSrc, danger: bool) -> impl IntoElement {
    view()
        .w(px(TILE))
        .h(px(TILE))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .child(
            svg(image)
                .w(px(TILE_ICON))
                .h(px(TILE_ICON))
                .text_color(if danger {
                    theme::accent_red()
                } else {
                    theme::text_secondary()
                }),
        )
}

/// Supporting copy: one step lighter than the muted foreground.
fn subtle_text() -> Hsla {
    theme::muted_foreground().opacity(0.9)
}

/// Explanatory copy under a row title: one step lighter again.
fn subtitle_text() -> Hsla {
    theme::muted_foreground().opacity(0.8)
}

/// A trailing chevron or a quiet glyph.
fn subtle_icon() -> Hsla {
    theme::muted_foreground().opacity(0.75)
}

/// The soft lift under a settings card: a wide, very low-opacity shadow.
fn card_shadow() -> BoxShadow {
    BoxShadow {
        offset_x: px(0.0),
        offset_y: px(3.0),
        blur: px(14.0),
        spread: px(0.0),
        color: nsh_core::element::Color::rgba(0, 0, 0, 6),
        inset: false,
    }
}

/// The stronger lift under a dialog.
fn dialog_shadow() -> BoxShadow {
    BoxShadow {
        offset_x: px(0.0),
        offset_y: px(10.0),
        blur: px(36.0),
        spread: px(0.0),
        color: nsh_core::element::Color::rgba(0, 0, 0, 32),
        inset: false,
    }
}

fn divider() -> impl IntoElement {
    divider_from(16.0)
}

fn divider_from(start: f32) -> impl IntoElement {
    view().w_full().pl(px(start)).child(
        view()
            .w_full()
            .h(px(1.0))
            .bg(theme::border_subtle().opacity(0.35)),
    )
}

fn footer_note(message: &'static str) -> impl IntoElement {
    text(message)
        .w_full()
        .px(px(16.0))
        .text_size(px(theme::font_hint()))
        .line_height(theme::line_height_for(theme::font_hint()))
        .text_color(subtitle_text)
}

fn section_label(label: &'static str) -> impl IntoElement {
    text(label)
        .w_full()
        .py(px(8.0))
        .text_size(px(theme::font_caption()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(subtle_text)
}

fn navigate(page: Signal<SettingsPage>, target: SettingsPage) -> impl Fn() + 'static {
    move || page.set(target)
}

fn appearance_label(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::System => copy::text("System", "跟随系统", "跟隨系統"),
        Appearance::Light => copy::text("Light", "浅色", "淺色"),
        Appearance::Dark => copy::text("Dark", "深色", "深色"),
    }
}

fn language_label(language: copy::LanguagePreference) -> &'static str {
    use copy::LanguagePreference as Choice;
    match language {
        Choice::System => copy::text("System", "跟随系统", "跟隨系統"),
        Choice::English => "English",
        Choice::SimplifiedChinese => "简体中文",
        Choice::TraditionalChinese => "繁體中文",
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

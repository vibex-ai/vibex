//! The pairing page: the phone's front door while no runtime is connected.
//!
//! The GPUI client's pairing page (`apps/mobile/src/app.rs`, `render_pairing`)
//! has three ways in — scan the desktop's QR code, find a runtime on the same
//! network, or paste a pairing link — plus the runtimes this phone saved
//! before. The structure, the copy and the spacing are the same here; what
//! changes is who draws them: `nsh` mounts platform views, so the camera, the
//! keyboard, the sheets and the scrolling are the platform's.
//!
//! The page owns only transient view state (which sheet is open, what is
//! typed). The saved runtimes, their reachability, the claim in flight and its
//! error are signals the app's pairing service owns; this module binds them.
//! The callbacks are the service's seams: the screen never scans, dials or
//! stores anything itself.

use std::rc::Rc;

use nsh_base::{ButtonElement, SheetEvent, button, sheet};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::{
    copy, theme,
    ui::{action, brand, field},
};

/// The page's top padding above the wordmark, matching the GPUI client.
const PAGE_TOP_PAD: f32 = 88.0;
/// The content column's width on a phone.
const CARD_WIDTH: f32 = 300.0;
/// The height of a saved runtime's row.
const RUNTIME_ROW_HEIGHT: f32 = 56.0;

/// How a saved runtime answers right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeStatus {
    /// It answered the last probe.
    Reachable,
    /// It did not answer the last probe.
    Unreachable,
    /// The phone is asking.
    Probing,
}

impl RuntimeStatus {
    /// The color of the row's leading dot.
    fn dot(self) -> Hsla {
        match self {
            RuntimeStatus::Reachable => theme::color_token("chart-1"),
            RuntimeStatus::Unreachable => theme::accent_red(),
            RuntimeStatus::Probing => theme::muted_foreground(),
        }
    }
}

/// A runtime this phone paired with before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedRuntime {
    /// The credential bundle's id.
    pub id: String,
    /// What the row shows.
    pub name: String,
    /// How it answers right now.
    pub status: RuntimeStatus,
    /// The runtime's address, for its details.
    pub address: String,
    /// The runtime's server id, for its details.
    pub server_id: String,
}

/// What a saved runtime's details page shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDetails {
    pub id: String,
    pub name: String,
    pub address: String,
    pub server_id: String,
    pub device_name: Option<String>,
    pub last_connected: Option<String>,
    pub added: Option<String>,
}

/// A runtime the local network found and the user may pair with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyRuntime {
    /// The discovery key, stable while the runtime stays visible.
    pub key: String,
    /// The name the desktop advertises.
    pub name: String,
}

/// A callback the app supplies for a page action.
type Action = Rc<dyn Fn()>;
/// A claim of an entry and the server address a bare code needs.
type Connect = Rc<dyn Fn(&str, &str)>;
/// Fills the sheet's entry field from the clipboard.
type Paste = Rc<dyn Fn(Signal<String>)>;
/// An action on a runtime the page names by id or discovery key.
type Pick = Rc<dyn Fn(&str)>;
/// A rename of a runtime, by id and the new name.
type Rename = Rc<dyn Fn(&str, &str)>;

/// What the page may do, and the state it shows.
///
/// Every callback is the service's: the screen never scans, dials or stores
/// anything itself. The app builds one of these over its pairing service.
#[derive(Clone)]
pub struct PairingHost {
    /// The runtimes this phone saved, most recent first.
    pub saved: Signal<Vec<SavedRuntime>>,
    /// The runtimes nearby discovery has found so far.
    pub nearby: Signal<Vec<NearbyRuntime>>,
    /// A nearby session waits for the desktop's approval.
    pub nearby_waiting: Signal<bool>,
    /// The code the desktop shows while it waits.
    pub nearby_code: Signal<Option<SharedString>>,
    pub on_cancel_nearby: Action,
    /// The nearby sheet closed: stop browsing.
    pub on_stop_nearby: Action,
    /// A pairing action is in flight (the buttons rest and say so).
    pub busy: Signal<bool>,
    /// What the last action failed with, shown under the entries.
    pub error: Signal<Option<SharedString>>,
    /// Open the camera to scan the desktop's QR code.
    pub on_scan: Action,
    /// Claim the typed or pasted entry; the second argument is the server
    /// address a bare code needs.
    pub on_connect: Connect,
    /// Read the clipboard into the sheet's entry field.
    pub on_paste: Paste,
    /// Start discovery for runtimes on the same network.
    pub on_find: Action,
    /// Connect to a saved runtime.
    pub on_pick_saved: Pick,
    /// Open a saved runtime's details.
    pub on_show_details: Pick,
    /// Remove a saved runtime's credential.
    pub on_remove_saved: Pick,
    /// Rename a saved runtime.
    pub on_rename_saved: Rename,
    /// The name field in the details sheet.
    pub rename_draft: Signal<String>,
    /// The page is the switcher over a live session: no wordmark, a back
    /// button, and the Switch/Network/Clipboard footer.
    pub switcher: Signal<bool>,
    pub on_close_switcher: Action,
    /// The details sheet's state.
    pub details: Signal<Option<HostDetails>>,
    pub on_close_details: Action,
    /// Pair with a runtime discovery found.
    pub on_pick_nearby: Pick,
}

/// The pairing page.
pub fn pairing(host: PairingHost) -> impl IntoElement {
    let manual_open = signal(false);
    let nearby_open = signal(false);
    let settings_open = signal(false);
    let switcher = host.switcher;
    let draft = signal(String::new());
    let server = signal(String::new());

    let saved = host.saved;
    let busy = host.busy;
    let error = host.error;

    let page = view()
        .w_full()
        .max_w(px(CARD_WIDTH))
        .flex_1()
        .flex_col()
        .child(view().w_full().items_center().child(brand::wordmark()))
        .child(when(move || saved.with(|saved| !saved.is_empty()), {
            let on_pick = host.on_pick_saved.clone();
            let on_details = host.on_show_details.clone();
            move || saved_runtimes(saved, on_pick.clone(), on_details.clone())
        }))
        .child(
            view()
                .flex_1()
                .flex_col()
                .justify_center()
                .child(when(move || saved.with(|saved| saved.is_empty()), {
                    let host = host.clone();
                    move || entries_stack(host.clone(), manual_open, nearby_open)
                }))
                .child(when(
                    move || busy.get(),
                    move || {
                        text(copy::text("Pairing...", "配对中…", "配對中…"))
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_muted)
                    },
                ))
                .child(when(
                    move || error.get().is_some(),
                    move || error_message(error),
                )),
        )
        .child(when(move || saved.with(|saved| !saved.is_empty()), {
            let host = host.clone();
            move || {
                if switcher.get() {
                    switcher_footer(host.clone(), manual_open, nearby_open).into_element()
                } else {
                    entries_row(host.clone(), manual_open, nearby_open).into_element()
                }
            }
        }));

    view()
        .flex_1()
        .relative()
        .items_center()
        .bg(theme::background)
        .px(px(theme::SPACING_XL))
        .pt_safe_area_plus(px(PAGE_TOP_PAD))
        .pb_safe_area_plus(px(theme::SPACING_LG))
        .child(page)
        .child(manual_sheet(host.clone(), manual_open, draft, server))
        .child(nearby_sheet(host.clone(), nearby_open))
        .child(details_sheet(host.clone()))
        .child(
            view()
                .absolute()
                .top(px(8.0))
                .right(px(8.0))
                .child(action::icon_button(
                    icon(include_str!("../../assets/icons/settings.svg")),
                    copy::text("Settings", "设置", "設定"),
                    move || settings_open.set(true),
                )),
        )
        .child(settings_sheet(settings_open))
        .child(when(move || switcher.get(), {
            let on_close = host.on_close_switcher.clone();
            move || {
                let on_close = on_close.clone();
                view()
                    .absolute()
                    .top_0()
                    .left(px(8.0))
                    .child(action::icon_button(
                        icon(include_str!("../../assets/icons/chevron-left.svg")),
                        copy::text("Back", "返回", "返回"),
                        move || on_close(),
                    ))
            }
        }))
}

/// The switcher's footer: back to the session, the network search, or the
/// clipboard fallback.
fn switcher_footer(
    host: PairingHost,
    manual_open: Signal<bool>,
    nearby_open: Signal<bool>,
) -> impl IntoElement {
    let on_close = host.on_close_switcher.clone();
    view()
        .flex_row()
        .w_full()
        .gap(px(theme::SPACING_SM))
        .py(px(theme::SPACING_MD))
        .child(
            action::primary(
                copy::text("Switch", "切换", "切換"),
                icon(include_str!("../../assets/icons/refresh.svg")),
                move || on_close(),
            )
            .flex_1(),
        )
        .child(
            action::secondary(
                copy::text("Network", "网络", "網路"),
                icon(include_str!("../../assets/icons/wifi-outlined.svg")),
                false,
                move || nearby_open.set(true),
            )
            .flex_1(),
        )
        .child(
            action::secondary(
                copy::text("Clipboard", "剪贴板", "剪貼簿"),
                icon(include_str!("../../assets/icons/clipboard-paste.svg")),
                false,
                move || manual_open.set(true),
            )
            .flex_1(),
        )
}

/// The first screen's settings sheet: the facts this build can state today.
fn settings_sheet(open: Signal<bool>) -> impl IntoElement {
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event {
                open.set(*next);
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
                    text(copy::text("Settings", "设置", "設定"))
                        .text_size(px(theme::FONT_HEADING))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(settings_row(
                    copy::text("Appearance", "外观", "外觀"),
                    copy::text("System", "跟随系统", "跟隨系統"),
                ))
                .child(
                    view()
                        .flex_row()
                        .gap(px(theme::SPACING_SM))
                        .child(
                            action::secondary(
                                copy::text("System", "系统", "系統"),
                                icon(include_str!("../../assets/icons/monitor.svg")),
                                false,
                                || {
                                    nsh_core::element::set_color_scheme(
                                        nsh_core::platform::Appearance::System,
                                    )
                                },
                            )
                            .flex_1(),
                        )
                        .child(
                            action::secondary(
                                copy::text("Light", "浅色", "淺色"),
                                icon(include_str!("../../assets/icons/image.svg")),
                                false,
                                || {
                                    nsh_core::element::set_color_scheme(
                                        nsh_core::platform::Appearance::Light,
                                    )
                                },
                            )
                            .flex_1(),
                        )
                        .child(
                            action::secondary(
                                copy::text("Dark", "深色", "深色"),
                                icon(include_str!("../../assets/icons/image.svg")),
                                false,
                                || {
                                    nsh_core::element::set_color_scheme(
                                        nsh_core::platform::Appearance::Dark,
                                    )
                                },
                            )
                            .flex_1(),
                        ),
                )
                .child(settings_row(
                    copy::text("Version", "版本", "版本"),
                    "Vibex 0.1.8",
                ))
                .child(
                    action::secondary(
                        copy::text("Close", "关闭", "關閉"),
                        icon(include_str!("../../assets/icons/x.svg")),
                        false,
                        move || open.set(false),
                    )
                    .w_full(),
                ),
        )
}

fn settings_row(label: &'static str, value: &'static str) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .items_center()
        .justify_between()
        .child(
            text(label)
                .text_size(px(theme::FONT_DETAIL))
                .text_color(theme::foreground),
        )
        .child(
            text(value)
                .text_size(px(theme::FONT_DETAIL))
                .text_color(theme::text_muted),
        )
}

/// A saved runtime's details, as a sheet.
fn details_sheet(host: PairingHost) -> impl IntoElement {
    let details = host.details;
    let rename_draft = host.rename_draft;
    let on_rename = host.on_rename_saved.clone();
    let open = computed(move || details.get().is_some());
    let on_close = host.on_close_details.clone();
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
                    text(copy::text("Runtime details", "运行时详情", "執行環境詳情"))
                        .text_size(px(theme::FONT_HEADING))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(
                    view()
                        .w_full()
                        .flex_row()
                        .items_end()
                        .gap(px(theme::SPACING_SM))
                        .child(field::text_field(
                            rename_draft,
                            copy::text("Name", "名称", "名稱"),
                        ))
                        .child({
                            let on_rename = on_rename.clone();
                            action::secondary(
                                copy::text("Save", "保存", "儲存"),
                                icon(include_str!("../../assets/icons/check.svg")),
                                false,
                                move || {
                                    if let Some(details) = details.get_untracked() {
                                        on_rename(&details.id, &rename_draft.get_untracked());
                                    }
                                },
                            )
                        }),
                )
                .child(details_field(
                    copy::text("Type", "类型", "類型"),
                    move || {
                        details.with(|details| {
                            details
                                .as_ref()
                                .map(|details| {
                                    if details.address.starts_with("http") {
                                        copy::text("Desktop runtime", "桌面运行时", "桌面執行環境")
                                    } else {
                                        copy::text("Runtime", "运行时", "執行環境")
                                    }
                                    .to_owned()
                                })
                                .unwrap_or_default()
                        })
                    },
                ))
                .child(details_field(
                    copy::text("Address", "地址", "位址"),
                    move || {
                        details.with(|details| {
                            details
                                .as_ref()
                                .map(|details| details.address.clone())
                                .unwrap_or_default()
                        })
                    },
                ))
                .child(details_field(
                    copy::text("Server ID", "服务器 ID", "伺服器 ID"),
                    move || {
                        details.with(|details| {
                            details
                                .as_ref()
                                .map(|details| details.server_id.clone())
                                .unwrap_or_default()
                        })
                    },
                ))
                .child(details_field(
                    copy::text("Client identity", "客户端身份", "用戶端身分"),
                    || "vibex-mobile".to_owned(),
                ))
                .child(when(
                    move || {
                        details.with(|details| {
                            details
                                .as_ref()
                                .is_some_and(|details| details.device_name.is_some())
                        })
                    },
                    move || {
                        details_field(
                            copy::text("Device name", "设备名称", "裝置名稱"),
                            move || {
                                details.with(|details| {
                                    details
                                        .as_ref()
                                        .and_then(|details| details.device_name.clone())
                                        .unwrap_or_default()
                                })
                            },
                        )
                    },
                ))
                .child(when(
                    move || {
                        details.with(|details| {
                            details
                                .as_ref()
                                .is_some_and(|details| details.added.is_some())
                        })
                    },
                    move || {
                        details_field(
                            copy::text("Added", "添加时间", "加入時間"),
                            move || {
                                details.with(|details| {
                                    details
                                        .as_ref()
                                        .and_then(|details| details.added.clone())
                                        .unwrap_or_default()
                                })
                            },
                        )
                    },
                ))
                .child(when(
                    move || {
                        details.with(|details| {
                            details
                                .as_ref()
                                .is_some_and(|details| details.last_connected.is_some())
                        })
                    },
                    move || {
                        details_field(
                            copy::text("Last connected", "最近连接", "最近連線"),
                            move || {
                                details.with(|details| {
                                    details
                                        .as_ref()
                                        .and_then(|details| details.last_connected.clone())
                                        .unwrap_or_default()
                                })
                            },
                        )
                    },
                ))
                .child({
                    let on_remove = host.on_remove_saved.clone();
                    action::secondary(
                        copy::text("Remove runtime", "移除运行时", "移除執行環境"),
                        icon(include_str!("../../assets/icons/x.svg")),
                        false,
                        move || {
                            if let Some(details) = details.get_untracked() {
                                on_remove(&details.id);
                            }
                        },
                    )
                    .w_full()
                }),
        )
}

fn details_field(label: &'static str, value: impl Fn() -> String + 'static) -> impl IntoElement {
    view()
        .w_full()
        .gap(px(2.0))
        .child(
            text(label)
                .text_size(px(theme::FONT_MICRO))
                .text_color(theme::text_muted),
        )
        .child(
            text(value)
                .text_size(px(theme::FONT_DETAIL))
                .text_color(theme::foreground),
        )
}

/// The last action's failure, worded for the person holding the phone.
fn error_message(error: Signal<Option<SharedString>>) -> impl IntoElement {
    text(move || error.get().unwrap_or_default())
        .text_size(px(theme::FONT_CAPTION))
        .text_color(theme::accent_red)
}

/// The three ways in, stacked in the middle of a page with nothing saved.
fn entries_stack(
    host: PairingHost,
    manual_open: Signal<bool>,
    nearby_open: Signal<bool>,
) -> impl IntoElement {
    view()
        .flex_col()
        .w_full()
        .gap(px(theme::SPACING_SM))
        .child(scan_entry(host.clone()).w_full())
        .child(find_entry(host.busy, nearby_open).w_full())
        .child(link_entry(host.busy, manual_open).w_full())
}

/// The same three ways as a row pinned under a page that has saved runtimes.
fn entries_row(
    host: PairingHost,
    manual_open: Signal<bool>,
    nearby_open: Signal<bool>,
) -> impl IntoElement {
    view()
        .flex_row()
        .w_full()
        .gap(px(theme::SPACING_SM))
        .py(px(theme::SPACING_MD))
        .child(scan_entry(host.clone()).flex_1())
        .child(find_entry(host.busy, nearby_open).flex_1())
        .child(link_entry(host.busy, manual_open).flex_1())
}

/// The primary entry: the camera.
fn scan_entry(host: PairingHost) -> ButtonElement {
    let on_scan = host.on_scan.clone();
    let busy = host.busy;
    action::primary(
        move || {
            if busy.get() {
                copy::text("Pairing...", "配对中…", "配對中…")
            } else {
                copy::text("Scan", "扫码", "掃碼")
            }
        },
        icon(include_str!("../../assets/icons/scan-line.svg")),
        move || {
            if !busy.get_untracked() {
                on_scan();
            }
        },
    )
    .disabled(move || busy.get())
    .disabled_style(|style| style.opacity(0.6))
}

/// Find a runtime on the same network.
fn find_entry(busy: Signal<bool>, nearby_open: Signal<bool>) -> ButtonElement {
    action::secondary(
        copy::text("Find", "查找", "尋找"),
        icon(include_str!("../../assets/icons/wifi-outlined.svg")),
        false,
        move || nearby_open.set(true),
    )
    .disabled(move || busy.get())
    .disabled_style(|style| style.opacity(0.4))
}

/// Paste a pairing link or type a code.
fn link_entry(busy: Signal<bool>, manual_open: Signal<bool>) -> ButtonElement {
    action::secondary(
        copy::text("Link", "链接", "連結"),
        icon(include_str!("../../assets/icons/clipboard-paste.svg")),
        false,
        move || manual_open.set(true),
    )
    .disabled(move || busy.get())
    .disabled_style(|style| style.opacity(0.4))
}

/// The runtimes this phone saved, above the entries.
fn saved_runtimes(
    saved: Signal<Vec<SavedRuntime>>,
    on_pick: Rc<dyn Fn(&str)>,
    on_details: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    view()
        .flex_col()
        .w_full()
        .gap(px(theme::SPACING_SM))
        .pt(px(theme::SPACING_XL))
        .child(each(
            move || saved.get(),
            |runtime| runtime.id.clone(),
            move |runtime, _| saved_row(runtime, on_pick.clone(), on_details.clone()),
        ))
}

/// One saved runtime: a status dot, its name, and the display it runs on.
fn saved_row(
    runtime: ReadSignal<SavedRuntime>,
    on_pick: Rc<dyn Fn(&str)>,
    on_details: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    view()
        .w_full()
        .min_h(px(RUNTIME_ROW_HEIGHT))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_dim)
        .flex_row()
        .items_center()
        .child(
            button()
                .flex_1()
                .min_h(px(RUNTIME_ROW_HEIGHT))
                .pl(px(theme::SPACING_MD))
                .pr(px(theme::SPACING_SM))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .active(|style| style.opacity(0.85))
                .on_press(move || on_pick(&runtime.get_untracked().id))
                .child(when(
                    move || runtime.get().status == RuntimeStatus::Probing,
                    || {
                        text("…")
                            .w(px(8.0))
                            .flex_shrink_0()
                            .text_size(px(theme::FONT_MICRO))
                            .text_color(theme::text_muted)
                    },
                ))
                .child(when(
                    move || runtime.get().status != RuntimeStatus::Probing,
                    move || {
                        view()
                            .w(px(8.0))
                            .h(px(8.0))
                            .flex_shrink_0()
                            .rounded(px(4.0))
                            .bg(move || runtime.get().status.dot())
                    },
                ))
                .child(
                    text(move || runtime.get().name)
                        .flex_1()
                        .truncate()
                        .text_size(px(theme::FONT_BODY))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text_secondary),
                )
                .child(
                    svg(icon(include_str!("../../assets/icons/monitor.svg")))
                        .w(px(theme::ICON_SM))
                        .h(px(theme::ICON_SM))
                        .flex_shrink_0()
                        .text_color(theme::text_muted),
                ),
        )
        .child({
            button()
                .min_h(px(theme::TOUCH_TARGET))
                .min_w(px(theme::TOUCH_TARGET))
                .items_center()
                .justify_center()
                .active(|style| style.opacity(0.7))
                .accessibility_label(copy::text("Runtime actions", "运行时操作", "執行環境操作"))
                .on_press(move || on_details(&runtime.get_untracked().id))
                .child(
                    text("⋯")
                        .text_size(px(theme::FONT_HEADING))
                        .text_color(theme::text_muted),
                )
        })
}

/// The manual entry sheet.
fn manual_sheet(
    host: PairingHost,
    open: Signal<bool>,
    draft: Signal<String>,
    server: Signal<String>,
) -> impl IntoElement {
    let paste = host.on_paste.clone();
    let connect = host.on_connect.clone();
    let busy = host.busy;
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event {
                open.set(*next);
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
                    text(copy::text("Link a runtime", "链接运行时", "連結執行環境"))
                        .text_size(px(theme::FONT_HEADING))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(
                    text(copy::text(
                        "Paste a code or link",
                        "粘贴配对码或链接",
                        "貼上配對碼或連結",
                    ))
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_muted),
                )
                .child(when(
                    move || needs_address(&draft.get()),
                    move || {
                        view()
                            .gap(px(theme::SPACING_XS))
                            .child(
                                text(copy::text("Server address", "服务器地址", "伺服器位址"))
                                    .text_size(px(theme::FONT_CAPTION))
                                    .text_color(theme::text_muted),
                            )
                            .child(field::text_field(server, "https://desktop.example:8443"))
                    },
                ))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_SM))
                        .child(field::text_field(draft, "vibex://…").flex_1())
                        .child({
                            let paste = paste.clone();
                            action::secondary(
                                copy::text("Paste", "粘贴", "貼上"),
                                icon(include_str!("../../assets/icons/clipboard-paste.svg")),
                                false,
                                move || paste(draft),
                            )
                        }),
                )
                .child(
                    text(copy::text(
                        "Paste the whole line, including the vibex:// prefix",
                        "请粘贴完整的一行，包含 vibex:// 前缀",
                        "請貼上完整的一行，包含 vibex:// 前綴",
                    ))
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_muted),
                )
                .child({
                    let connect = connect.clone();
                    action::primary(
                        move || {
                            if busy.get() {
                                copy::text("Connecting…", "连接中…", "連線中…")
                            } else {
                                copy::text("Connect", "连接", "連線")
                            }
                        },
                        icon(include_str!("../../assets/icons/plug-zap.svg")),
                        move || {
                            if busy.get_untracked() {
                                return;
                            }
                            let entry = draft.get_untracked();
                            let address = server.get_untracked();
                            connect(entry.trim(), address.trim());
                            open.set(false);
                        },
                    )
                    .w_full()
                    .disabled(move || busy.get() || draft.with(|entry| entry.trim().is_empty()))
                    .disabled_style(|style| style.opacity(0.4))
                }),
        )
}

/// The nearby discovery sheet.
fn nearby_sheet(host: PairingHost, open: Signal<bool>) -> impl IntoElement {
    let nearby = host.nearby;
    let waiting = host.nearby_waiting;
    let code = host.nearby_code;
    let on_find = host.on_find.clone();
    let on_pick = host.on_pick_nearby.clone();
    let on_cancel = host.on_cancel_nearby.clone();
    let on_stop = host.on_stop_nearby.clone();
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event {
                open.set(*next);
                if !next {
                    on_stop();
                }
            }
        })
        .on_open(move || on_find())
        .child(
            view()
                .w_full()
                .gap(px(theme::SPACING_SM))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(16.0))
                .child(
                    text(copy::text("Nearby", "附近", "附近"))
                        .text_size(px(theme::FONT_HEADING))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(when(
                    move || waiting.get(),
                    move || {
                        view()
                            .w_full()
                            .gap(px(theme::SPACING_SM))
                            .items_center()
                            .child(
                                text(copy::text(
                                    "Confirm this code on your computer",
                                    "请在电脑上确认这组代码",
                                    "請在電腦上確認這組代碼",
                                ))
                                .text_size(px(theme::FONT_CAPTION))
                                .text_color(theme::text_muted),
                            )
                            .child(
                                text(move || code.get().unwrap_or_default())
                                    .text_size(px(32.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::foreground),
                            )
                            .child({
                                let on_cancel = on_cancel.clone();
                                action::secondary(
                                    copy::text("Cancel", "取消", "取消"),
                                    icon(include_str!("../../assets/icons/x.svg")),
                                    false,
                                    move || on_cancel(),
                                )
                                .w_full()
                            })
                    },
                ))
                .child(when(
                    move || !waiting.get() && nearby.with(|nearby| nearby.is_empty()),
                    || {
                        view().h(px(56.0)).items_center().justify_center().child(
                            text(copy::text("Searching...", "正在搜索…", "正在搜尋…"))
                                .text_size(px(theme::FONT_DETAIL))
                                .text_color(theme::text_muted),
                        )
                    },
                ))
                .child(when(move || !waiting.get(), {
                    let on_pick = on_pick.clone();
                    move || {
                        each(move || nearby.get(), |runtime| runtime.key.clone(), {
                            let on_pick = on_pick.clone();
                            move |runtime, _| nearby_row(runtime, on_pick.clone(), open)
                        })
                    }
                })),
        )
}

/// One runtime discovery found.
fn nearby_row(
    runtime: ReadSignal<NearbyRuntime>,
    on_pick: Rc<dyn Fn(&str)>,
    open: Signal<bool>,
) -> impl IntoElement {
    button()
        .on_press(move || {
            on_pick(&runtime.get_untracked().key);
            open.set(false);
        })
        .w_full()
        .min_h(px(58.0))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_MD))
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .active(|style| style.opacity(0.85))
        .child(
            svg(icon(include_str!("../../assets/icons/monitor.svg")))
                .w(px(theme::ICON_MD))
                .h(px(theme::ICON_MD))
                .flex_shrink_0()
                .text_color(theme::foreground),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .flex_col()
                .child(
                    text(move || runtime.get().name)
                        .truncate()
                        .text_size(px(theme::FONT_DETAIL))
                        .text_color(theme::foreground),
                )
                .child(
                    text(copy::text("Same Wi-Fi", "同一 Wi-Fi", "同一個 Wi-Fi"))
                        .text_size(px(theme::FONT_MICRO))
                        .text_color(theme::text_muted),
                ),
        )
        .child(
            text(copy::text("Pair", "配对", "配對"))
                .text_size(px(theme::FONT_DETAIL))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::foreground),
        )
}

/// Whether the entry is a bare code, which needs the server's address too.
///
/// The pairing crate owns the authoritative rule for claiming
/// (`vibex_mobile_pairing::is_pairing_link`); this is the page's copy of the
/// shapes it accepts, used only to decide whether the address field shows.
fn needs_address(entry: &str) -> bool {
    let entry = entry.trim();
    !entry.is_empty()
        && !(entry.starts_with("vibex://")
            || entry.contains("#/pair/")
            || entry.contains("#/code/"))
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

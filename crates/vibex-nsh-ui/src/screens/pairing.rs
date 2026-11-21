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

/// The page's top padding above the wordmark.
const PAGE_TOP_PAD: f32 = 152.0;
/// The switcher opens over a live connection and carries no wordmark, so its
/// rows start one step under the back button.
const SWITCHER_TOP_PAD: f32 = 56.0;
/// The content column's width on a phone.
const CARD_WIDTH: f32 = 300.0;
/// The gap under the wordmark before the saved runtimes.
///
/// The GPUI lockup's line box ends lower than `nsh`'s text box, so the card
/// list's top edge is kept here rather than by the brand's own bottom margin.
const WORDMARK_BOTTOM_GAP: f32 = 66.0;
/// The height of a saved runtime's row.
const RUNTIME_ROW_HEIGHT: f32 = 52.0;
/// How much of the screen the saved list may take on the first screen.
const RUNTIMES_MAX_HEIGHT: f32 = 280.0;
/// The switcher has no wordmark to leave room for, so its list keeps more.
const SWITCHER_RUNTIMES_MAX_HEIGHT: f32 = 400.0;
/// The height of a sheet's header.
const SHEET_HEADER_HEIGHT: f32 = 56.0;

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

    /// The words behind the dot, for the row's accessible name.
    fn label(self) -> &'static str {
        match self {
            RuntimeStatus::Reachable => copy::text("Online", "在线", "線上"),
            RuntimeStatus::Unreachable => copy::text("Offline", "离线", "離線"),
            RuntimeStatus::Probing => copy::text("Checking", "检查中", "檢查中"),
        }
    }
}

/// What kind of process hosts a runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RuntimeKind {
    /// The embedded runtime inside a Vibex desktop shell.
    Desktop,
    /// A standalone `vibex-server` process.
    Headless,
    /// The peer predates the field, or has not answered yet.
    #[default]
    Unknown,
}

impl RuntimeKind {
    /// The localized name shown on the details page.
    pub fn label(self) -> &'static str {
        match self {
            RuntimeKind::Desktop => copy::text("Desktop", "桌面端", "桌面版"),
            RuntimeKind::Headless => copy::text("Headless server", "无头服务器", "無頭伺服器"),
            RuntimeKind::Unknown => copy::text("Runtime", "运行时", "執行環境"),
        }
    }

    /// The glyph a runtime's kind wears on its row.
    fn icon(self) -> &'static str {
        match self {
            RuntimeKind::Desktop => include_str!("../../assets/icons/monitor.svg"),
            RuntimeKind::Headless | RuntimeKind::Unknown => {
                include_str!("../../assets/icons/server.svg")
            }
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
    /// What hosts it.
    pub kind: RuntimeKind,
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
    pub kind: RuntimeKind,
    pub address: String,
    pub server_id: String,
    pub device_name: Option<String>,
    /// The localized "Last connected …" line.
    pub last_connected: Option<String>,
    /// The localized "Added …" line.
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

/// What the nearby sheet shows while it browses, waits or explains a failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NearbyPhase {
    /// Browsing; a list of candidates may already be arriving.
    Searching,
    /// The browse ran and found nothing.
    Empty,
    /// A candidate is being checked before the desktop is asked to approve.
    Validating(SharedString),
    /// The desktop is waiting for its operator to confirm `code`.
    Waiting {
        name: SharedString,
        code: SharedString,
        expires_at_ms: i64,
    },
    /// The platform refused local-network access.
    PermissionDenied,
    /// The computer declined the request.
    Rejected,
    /// The request expired before it was approved.
    Expired,
    /// Something else failed; the message is already worded for the reader.
    Failed(SharedString),
}

impl NearbyPhase {
    /// Whether the header offers the retry action.
    fn retryable(&self) -> bool {
        matches!(
            self,
            NearbyPhase::Empty
                | NearbyPhase::PermissionDenied
                | NearbyPhase::Rejected
                | NearbyPhase::Expired
                | NearbyPhase::Failed(_)
        )
    }

    /// Whether a pairing request is in flight.
    fn busy(&self) -> bool {
        matches!(
            self,
            NearbyPhase::Validating(_) | NearbyPhase::Waiting { .. }
        )
    }
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
    /// What the nearby sheet shows.
    pub nearby_phase: Signal<NearbyPhase>,
    /// The nearby sheet closed: stop browsing.
    pub on_stop_nearby: Action,
    /// Browse again after a failed or empty search.
    pub on_retry_nearby: Action,
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
    /// The runtime whose actions sheet is open.
    pub actions_id: Signal<Option<SharedString>>,
    /// The runtime whose rename or remove dialog is open.
    pub dialog_id: Signal<Option<SharedString>>,
    /// The rename dialog is open.
    pub rename_open: Signal<bool>,
    /// The remove dialog is open.
    pub remove_open: Signal<bool>,
    pub on_show_actions: Pick,
    pub on_close_actions: Action,
    pub on_open_rename: Pick,
    pub on_close_rename: Action,
    pub on_open_remove: Pick,
    pub on_close_remove: Action,
    /// Open a saved runtime's details page.
    pub on_show_details: Pick,
    /// Remove a saved runtime's credential (the page confirms first).
    pub on_remove_saved: Pick,
    /// Rename a saved runtime.
    pub on_rename_saved: Rename,
    /// The name field in the details sheet.
    pub rename_draft: Signal<String>,
    /// The page is the switcher over a live session: no wordmark, a back
    /// button, and the Switch/Network/Clipboard footer.
    pub switcher: Signal<bool>,
    pub on_close_switcher: Action,
    /// The connected runtime's name, when there is one.
    pub connected_label: Signal<Option<SharedString>>,
    /// The id of the connected runtime, for the selected row and its actions.
    pub active_id: Signal<Option<SharedString>>,
    /// Leave the connected runtime.
    pub on_disconnect: Action,
    /// Remove every saved runtime's credential.
    pub on_clear_runtimes: Action,
    /// The details page's state.
    pub details: Signal<Option<HostDetails>>,
    pub on_close_details: Action,
    /// Pair with a runtime discovery found.
    pub on_pick_nearby: Pick,
    /// Opens the settings sheet over the page.
    pub on_open_settings: Action,
    /// Opens this app's system settings page (for a denied permission).
    pub on_open_platform_settings: Action,
}

/// The pairing page.
pub fn pairing(host: PairingHost) -> impl IntoElement {
    let manual_open = signal(false);
    let nearby_open = signal(false);
    let actions_id = host.actions_id;
    let rename_open = host.rename_open;
    let remove_open = host.remove_open;
    let switcher = host.switcher;
    let activity_phase = signal(0u32);
    let clock = signal(now_ms());
    if !crate::motion::reduce_motion() {
        spawn(async move {
            let mut frame = 0;
            loop {
                nsh_core::element::timer(std::time::Duration::from_millis(100)).await;
                frame = (frame + 1) % 8;
                activity_phase.set(frame);
            }
        });
    }
    // The verification code's countdown is read from here; a second is the
    // only resolution the line shows.
    spawn(async move {
        loop {
            nsh_core::element::timer(std::time::Duration::from_secs(1)).await;
            clock.set(now_ms());
        }
    });
    let draft = signal(String::new());
    let server = signal(String::new());
    let on_open_settings = host.on_open_settings.clone();

    let saved = host.saved;
    let error = host.error;

    let page = view()
        .w_full()
        .max_w(px(CARD_WIDTH))
        .flex_1()
        .flex_col()
        .pt(px(if switcher.get() {
            SWITCHER_TOP_PAD
        } else {
            PAGE_TOP_PAD
        }))
        .child(when(
            move || !switcher.get(),
            || {
                view()
                    .w_full()
                    .items_center()
                    .mb(px(WORDMARK_BOTTOM_GAP))
                    .child(brand::wordmark())
            },
        ))
        .child(when(move || saved.with(|saved| !saved.is_empty()), {
            let on_pick = host.on_pick_saved.clone();
            let on_actions = host.on_show_actions.clone();
            move || {
                saved_runtimes(
                    saved,
                    on_pick.clone(),
                    on_actions.clone(),
                    activity_phase,
                    switcher,
                )
            }
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
                    move || {
                        error.get().is_some()
                            && !manual_open.get()
                            && !nearby_open.get()
                            && actions_id.with(|id| id.is_none())
                    },
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
        .bg(theme::block_page)
        .px(px(theme::SPACING_XL))
        .pt_safe_area()
        .pb_safe_area_plus(px(theme::SPACING_LG))
        .child(page)
        .child(manual_sheet(host.clone(), manual_open, draft, server))
        .child(nearby_sheet(host.clone(), nearby_open, clock))
        .child(actions_sheet(host.clone()))
        .child(when(move || rename_open.get(), {
            let host = host.clone();
            move || rename_dialog(host.clone(), host.dialog_id, host.rename_open)
        }))
        .child(when(move || remove_open.get(), {
            let host = host.clone();
            move || remove_dialog(host.clone(), host.dialog_id, host.remove_open)
        }))
        .child(when(
            move || host.details.with(|details| details.is_some()),
            {
                let host = host.clone();
                move || details_page(host.clone())
            },
        ))
        .child(
            view()
                .absolute()
                .top_0()
                .right(px(8.0))
                .pt_safe_area()
                .child(when(move || !switcher.get(), {
                    let open_settings = on_open_settings.clone();
                    let busy = host.busy;
                    move || {
                        let open_settings = open_settings.clone();
                        action::floating_icon(
                            icon(include_str!("../../assets/icons/settings.svg")),
                            copy::text("Settings", "设置", "設定"),
                            move || open_settings.as_ref()(),
                        )
                        .disabled(move || busy.get())
                        .disabled_style(|style| style.opacity(0.4))
                    }
                })),
        )
        .child(when(move || switcher.get(), {
            let on_close = host.on_close_switcher.clone();
            let busy = host.busy;
            move || {
                let on_close = on_close.clone();
                view()
                    .absolute()
                    .top_0()
                    .left(px(8.0))
                    .pt_safe_area()
                    .child(
                        action::icon_button(
                            icon(include_str!("../../assets/icons/chevron-left.svg")),
                            copy::text("Back", "返回", "返回"),
                            move || on_close(),
                        )
                        .disabled(move || busy.get())
                        .disabled_style(|style| style.opacity(0.4)),
                    )
            }
        }))
}

/// The switcher's footer: the same three ways in the page itself offers.
fn switcher_footer(
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
    action::block(
        copy::text("Find", "查找", "尋找"),
        icon(include_str!("../../assets/icons/wifi-outlined.svg")),
        move || nearby_open.set(true),
    )
    .disabled(move || busy.get())
    .disabled_style(|style| style.opacity(0.4))
}

/// Paste a pairing link or type a code.
fn link_entry(busy: Signal<bool>, manual_open: Signal<bool>) -> ButtonElement {
    action::block(
        copy::text("Link", "链接", "連結"),
        icon(include_str!("../../assets/icons/clipboard-paste.svg")),
        move || manual_open.set(true),
    )
    .disabled(move || busy.get())
    .disabled_style(|style| style.opacity(0.4))
}

/// The last action's failure, worded for the person holding the phone.
fn error_message(error: Signal<Option<SharedString>>) -> impl IntoElement {
    text(move || error.get().unwrap_or_default())
        .text_size(px(theme::font_caption()))
        .text_color(theme::accent_red)
}

/// The runtimes this phone saved, above the entries.
fn saved_runtimes(
    saved: Signal<Vec<SavedRuntime>>,
    on_pick: Rc<dyn Fn(&str)>,
    on_actions: Rc<dyn Fn(&str)>,
    activity_phase: Signal<u32>,
    switcher: Signal<bool>,
) -> impl IntoElement {
    view().flex_col().w_full().pb(px(theme::SPACING_MD)).child(
        scroll()
            .max_h(move || {
                px(if switcher.get() {
                    SWITCHER_RUNTIMES_MAX_HEIGHT
                } else {
                    RUNTIMES_MAX_HEIGHT
                })
            })
            .child(
                view()
                    .flex_col()
                    .w_full()
                    .gap(px(theme::SPACING_SM))
                    .child(each(
                        move || saved.get(),
                        |runtime| runtime.id.clone(),
                        move |runtime, _| {
                            saved_row(runtime, on_pick.clone(), on_actions.clone(), activity_phase)
                        },
                    )),
            ),
    )
}

/// One saved runtime: a status dot, its name, the kind it is, and its actions.
fn saved_row(
    runtime: ReadSignal<SavedRuntime>,
    on_pick: Rc<dyn Fn(&str)>,
    on_actions: Rc<dyn Fn(&str)>,
    activity_phase: Signal<u32>,
) -> impl IntoElement {
    let accessible = format!(
        "{} · {} · {}",
        runtime.get_untracked().name,
        runtime.get_untracked().kind.label(),
        runtime.get_untracked().status.label()
    );
    view()
        .w_full()
        .min_h(px(RUNTIME_ROW_HEIGHT))
        .rounded(px(theme::RADIUS_CARD))
        .shadow_xs()
        .bg(theme::block_surface)
        .flex_row()
        .items_center()
        .pl(px(theme::SPACING_MD))
        .pr(px(theme::SPACING_XS))
        .py(px(theme::SPACING_XS))
        .gap(px(theme::SPACING_SM))
        .child(
            button()
                .flex_1()
                .min_h(px(theme::TOUCH_TARGET))
                .min_w(px(0.0))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .active(|style| style.opacity(0.85))
                .accessibility_label(accessible.clone())
                .on_press(move || on_pick(&runtime.get_untracked().id))
                .child(when(
                    move || runtime.get().status == RuntimeStatus::Probing,
                    move || activity_indicator(activity_phase, theme::text_muted()),
                ))
                .child(when(
                    move || runtime.get().status != RuntimeStatus::Probing,
                    move || {
                        view()
                            .w(px(20.0))
                            .items_center()
                            .justify_center()
                            .flex_shrink_0()
                            .child(
                                view()
                                    .w(px(8.0))
                                    .h(px(8.0))
                                    .rounded(px(4.0))
                                    .bg(move || runtime.get().status.dot()),
                            )
                    },
                ))
                .child(
                    text(move || runtime.get().name)
                        .flex_1()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(theme::font_body()))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text_secondary),
                )
                .child(
                    svg(icon(runtime.get_untracked().kind.icon()))
                        .w(px(theme::ICON_SM))
                        .h(px(theme::ICON_SM))
                        .flex_shrink_0()
                        .text_color(theme::text_muted),
                ),
        )
        .child({
            button()
                .w(px(34.0))
                .min_h(px(theme::TOUCH_TARGET))
                .items_center()
                .justify_center()
                .active(|style| style.opacity(0.7))
                .accessibility_label(copy::text("Runtime actions", "运行时操作", "執行環境操作"))
                .on_press(move || on_actions(&runtime.get_untracked().id))
                .child(
                    svg(icon(include_str!(
                        "../../assets/icons/ellipsis-vertical.svg"
                    )))
                    .w(px(theme::ICON_SM))
                    .h(px(theme::ICON_SM))
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
    let error = host.error;
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(move || !busy.get())
        .close_on_backdrop_press(move || !busy.get())
        .close_on_escape(move || !busy.get())
        .grabber(true)
        .grabber_style(|style| {
            style
                .self_center()
                .w(px(theme::SHEET_GRABBER_WIDTH))
                .h(px(theme::SHEET_GRABBER_HEIGHT))
                .mt(px(theme::SPACING_SM))
                .mb(px(theme::SPACING_XS))
                .rounded(px(theme::SHEET_GRABBER_HEIGHT / 2.0))
                .bg(theme::muted_foreground().opacity(0.55))
        })
        .backdrop_style(|style| {
            style.bg(nsh_core::color::black().opacity(theme::SHEET_BACKDROP_OPACITY))
        })
        .popup_style(|style| {
            style
                .rounded_t(px(theme::RADIUS_SHEET))
                .border_t_1()
                .border_color(|| theme::border_adaptive())
                .bg(|| theme::card_adaptive())
        })
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event {
                open.set(*next);
            }
        })
        .child(
            view()
                .w_full()
                .flex_col()
                .child(sheet_header(
                    copy::text(
                        "Paste a code or link",
                        "粘贴配对码或链接",
                        "貼上配對碼或連結",
                    ),
                    view(),
                    move || open.set(false),
                    busy,
                ))
                .child(
                    view()
                        .w_full()
                        .flex_col()
                        .gap(px(theme::SPACING_SM))
                        .px(px(theme::SPACING_MD))
                        .pt(px(theme::SPACING_MD))
                        .pb_safe_area_plus(px(theme::SPACING_LG))
                        .child(when(
                            move || needs_address(&draft.get()),
                            move || {
                                view()
                                    .flex_col()
                                    .child(pairing_field_label(|| {
                                        copy::text("Server address", "服务器地址", "伺服器位址")
                                    }))
                                    .child(bordered_field(server, "https://desktop.example:8443"))
                            },
                        ))
                        .child(
                            view()
                                .flex_col()
                                .child(pairing_field_label(move || {
                                    if needs_address(&draft.get()) {
                                        copy::text("Pairing code", "配对码", "配對碼")
                                    } else {
                                        copy::text(
                                            "Paste a code or link",
                                            "粘贴配对码或链接",
                                            "貼上配對碼或連結",
                                        )
                                    }
                                }))
                                .child(
                                    view()
                                        .flex_row()
                                        .items_center()
                                        .h(px(theme::TOUCH_TARGET))
                                        .rounded(px(theme::RADIUS_CONTROL))
                                        .border_1()
                                        .border_color(theme::border_default)
                                        .child(
                                            field::text_field_flat(
                                                draft,
                                                copy::text(
                                                    "Pairing code or link",
                                                    "配对码或链接",
                                                    "配對碼或連結",
                                                ),
                                            )
                                            .flex_1()
                                            .min_w(px(0.0)),
                                        )
                                        .child({
                                            let paste = paste.clone();
                                            button()
                                                .h(px(theme::TOUCH_TARGET))
                                                .px(px(theme::SPACING_MD))
                                                .flex_row()
                                                .items_center()
                                                .gap(px(theme::SPACING_XS))
                                                .active(|style| style.opacity(0.7))
                                                .on_press(move || paste(draft))
                                                .child(
                                                    svg(icon(include_str!(
                                                        "../../assets/icons/clipboard-paste.svg"
                                                    )))
                                                    .w(px(theme::ICON_SM))
                                                    .h(px(theme::ICON_SM))
                                                    .text_color(theme::foreground),
                                                )
                                                .child(
                                                    text(copy::text("Paste", "粘贴", "貼上"))
                                                        .text_size(px(theme::font_detail()))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_color(theme::foreground),
                                                )
                                        }),
                                ),
                        )
                        .child(
                            text(copy::text(
                                "Paste the whole line, including the vibex:// prefix",
                                "请粘贴完整的一行，包含 vibex:// 前缀",
                                "請貼上完整的一行，包含 vibex:// 前綴",
                            ))
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::text_muted),
                        )
                        .child(when(
                            move || error.get().is_some(),
                            move || error_message(error),
                        ))
                        .child({
                            let connect = connect.clone();
                            connect_button(busy, move || {
                                if busy.get_untracked() {
                                    return;
                                }
                                let entry = draft.get_untracked();
                                let address = server.get_untracked();
                                connect(entry.trim(), address.trim());
                            })
                        }),
                ),
        )
}

/// The nearby discovery sheet.
fn nearby_sheet(host: PairingHost, open: Signal<bool>, clock: Signal<i64>) -> impl IntoElement {
    let nearby = host.nearby;
    let phase = host.nearby_phase;
    let error = host.error;
    let on_find = host.on_find.clone();
    let on_retry = host.on_retry_nearby.clone();
    let on_pick = host.on_pick_nearby.clone();
    let on_stop = host.on_stop_nearby.clone();
    let on_open_platform_settings = host.on_open_platform_settings.clone();
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(move || !phase.with(NearbyPhase::busy))
        .close_on_backdrop_press(move || !phase.with(NearbyPhase::busy))
        .close_on_escape(move || !phase.with(NearbyPhase::busy))
        .grabber(true)
        .grabber_style(|style| {
            style
                .self_center()
                .w(px(theme::SHEET_GRABBER_WIDTH))
                .h(px(theme::SHEET_GRABBER_HEIGHT))
                .mt(px(theme::SPACING_SM))
                .mb(px(theme::SPACING_XS))
                .rounded(px(theme::SHEET_GRABBER_HEIGHT / 2.0))
                .bg(theme::muted_foreground().opacity(0.55))
        })
        .backdrop_style(|style| {
            style.bg(nsh_core::color::black().opacity(theme::SHEET_BACKDROP_OPACITY))
        })
        .popup_style(|style| {
            style
                .rounded_t(px(theme::RADIUS_SHEET))
                .border_t_1()
                .border_color(|| theme::border_adaptive())
                .bg(|| theme::card_adaptive())
        })
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
                .flex_col()
                .child({
                    let on_retry = on_retry.clone();
                    sheet_header(
                        copy::text(
                            "Find on this network",
                            "在同一网络查找",
                            "在同一網路上尋找",
                        ),
                        view().child(when(
                            move || phase.with(NearbyPhase::retryable),
                            {
                                let on_retry = on_retry.clone();
                                move || retry_action(on_retry.clone())
                            },
                        )),
                        move || open.set(false),
                        host.busy,
                    )
                })
                .child(
                    view()
                        .w_full()
                        .flex_col()
                        .px(px(theme::SPACING_MD))
                        .pb_safe_area_plus(px(theme::SPACING_LG))
                        .child(when(
                            move || error.get().is_some() && !phase.with(NearbyPhase::retryable),
                            move || error_message(error),
                        ))
                        .child(when(
                            move || {
                                matches!(
                                    phase.get(),
                                    NearbyPhase::Searching | NearbyPhase::Empty
                                ) && nearby.with(|nearby| nearby.is_empty())
                            },
                            move || {
                                view()
                                    .h(px(56.0))
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        text(move || {
                                            if matches!(phase.get(), NearbyPhase::Empty) {
                                                copy::text(
                                                    "No nearby desktops found",
                                                    "未找到附近的桌面端",
                                                    "找不到附近的桌面版",
                                                )
                                            } else {
                                                copy::text(
                                                    "Searching...",
                                                    "搜索中…",
                                                    "搜尋中…",
                                                )
                                            }
                                        })
                                        .text_size(px(theme::font_detail()))
                                        .text_color(theme::text_muted),
                                    )
                            },
                        ))
                        .child(when(
                            move || {
                                matches!(
                                    phase.get(),
                                    NearbyPhase::Searching | NearbyPhase::Empty
                                )
                            },
                            {
                                let on_pick = on_pick.clone();
                                move || {
                                    each(move || nearby.get(), |runtime| runtime.key.clone(), {
                                        let on_pick = on_pick.clone();
                                        move |runtime, index| {
                                            nearby_row(
                                                runtime,
                                                index.get_untracked(),
                                                nearby,
                                                on_pick.clone(),
                                            )
                                        }
                                    })
                                }
                            },
                        ))
                        .child(when(
                            move || matches!(phase.get(), NearbyPhase::Validating(_)),
                            move || {
                                let name = phase.with(|phase| match phase {
                                    NearbyPhase::Validating(name) => name.clone(),
                                    _ => SharedString::default(),
                                });
                                nearby_message(
                                    format!(
                                        "{} {name}…",
                                        copy::text("Checking", "检查", "檢查")
                                    ),
                                    false,
                                )
                            },
                        ))
                        .child(when(
                            move || matches!(phase.get(), NearbyPhase::Waiting { .. }),
                            move || nearby_waiting(phase, clock),
                        ))
                        .child(when(
                            move || matches!(phase.get(), NearbyPhase::PermissionDenied),
                            {
                                let on_open_settings = on_open_platform_settings.clone();
                                move || {
                                    view()
                                        .w_full()
                                        .flex_col()
                                        .items_center()
                                        .gap(px(theme::SPACING_SM))
                                        .child(nearby_message(
                                            copy::text(
                                                "Local network access is needed to find nearby computers",
                                                "查找附近的电脑需要局域网权限",
                                                "尋找附近的電腦需要區域網路權限",
                                            ),
                                            true,
                                        ))
                                        .child(
                                            action::secondary(
                                                copy::text(
                                                    "Open settings",
                                                    "打开设置",
                                                    "開啟設定",
                                                ),
                                                icon(include_str!(
                                                    "../../assets/icons/settings.svg"
                                                )),
                                                false,
                                                {
                                                    let open = on_open_settings.clone();
                                                    move || open.as_ref()()
                                                },
                                            )
                                            .h(px(theme::TOUCH_TARGET))
                                            .px(px(theme::SPACING_LG)),
                                        )
                                }
                            },
                        ))
                        .child(when(
                            move || matches!(phase.get(), NearbyPhase::Rejected),
                            || {
                                nearby_message(
                                    copy::text(
                                        "The computer rejected this pairing request",
                                        "电脑端拒绝了此次配对请求",
                                        "電腦版拒絕了此次配對請求",
                                    ),
                                    true,
                                )
                            },
                        ))
                        .child(when(
                            move || matches!(phase.get(), NearbyPhase::Expired),
                            || {
                                nearby_message(
                                    copy::text(
                                        "The nearby pairing window expired",
                                        "附近配对窗口已过期",
                                        "附近配對視窗已過期",
                                    ),
                                    true,
                                )
                            },
                        ))
                        .child(when(
                            move || matches!(phase.get(), NearbyPhase::Failed(_)),
                            move || {
                                let message = phase.with(|phase| match phase {
                                    NearbyPhase::Failed(message) => message.clone(),
                                    _ => SharedString::default(),
                                });
                                nearby_message(message, true)
                            },
                        )),
                ),
        )
}

/// The verification code while the desktop waits for its operator.
fn nearby_waiting(phase: Signal<NearbyPhase>, clock: Signal<i64>) -> impl IntoElement {
    let code = phase.with(|phase| match phase {
        NearbyPhase::Waiting { code, .. } => code.clone(),
        _ => SharedString::default(),
    });
    let remaining = move || {
        clock.with(|now| {
            (phase.with(|phase| match phase {
                NearbyPhase::Waiting { expires_at_ms, .. } => *expires_at_ms,
                _ => *now,
            }) - *now)
                .max(0)
                / 1000
        })
    };
    view()
        .w_full()
        .flex_col()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .child(
            text(move || {
                format!(
                    "{} {}",
                    copy::text("Waiting for", "等待", "等待"),
                    phase.with(|phase| match phase {
                        NearbyPhase::Waiting { name, .. } => name.clone(),
                        _ => SharedString::default(),
                    })
                )
            })
            .text_size(px(theme::font_detail()))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::foreground),
        )
        .child(
            text(copy::text(
                "Confirm the same code on the computer",
                "请在电脑上确认相同的验证码",
                "請在電腦上確認相同的驗證碼",
            ))
            .text_size(px(theme::font_detail()))
            .text_color(theme::text_muted),
        )
        .child(
            view().h(px(56.0)).items_center().justify_center().child(
                text(move || code.clone())
                    .text_size(px(theme::font_display()))
                    .font_weight(FontWeight::EXTRA_BOLD)
                    .text_color(theme::foreground),
            ),
        )
        .child(
            text(move || {
                format!(
                    "{} {}s",
                    copy::text("Expires in", "将在", "將於"),
                    remaining()
                )
            })
            .text_size(px(theme::font_micro()))
            .text_color(theme::text_muted),
        )
}

/// One runtime discovery found.
fn nearby_row(
    runtime: ReadSignal<NearbyRuntime>,
    index: usize,
    nearby: Signal<Vec<NearbyRuntime>>,
    on_pick: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    let last = move || nearby.with(|nearby| index + 1 == nearby.len());
    button()
        .on_press(move || on_pick(&runtime.get_untracked().key))
        .w_full()
        .min_h(px(58.0))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_MD))
        .border_b_1()
        .border_b_color(move || {
            if last() {
                theme::background_card()
            } else {
                theme::border_subtle()
            }
        })
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
                        .text_size(px(theme::font_detail()))
                        .text_color(theme::foreground),
                )
                .child(
                    text(copy::text("Same Wi-Fi", "同一 Wi-Fi", "同一個 Wi-Fi"))
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_muted),
                ),
        )
        .child(
            text(copy::text("Pair", "配对", "配對"))
                .text_size(px(theme::font_detail()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::foreground),
        )
}

/// The runtime actions sheet: switch, rename, details, remove.
fn actions_sheet(host: PairingHost) -> impl IntoElement {
    let saved = host.saved;
    let active = host.active_id;
    let actions_id = host.actions_id;
    let on_pick = host.on_pick_saved.clone();
    let on_details = host.on_show_details.clone();
    let on_open_rename = host.on_open_rename.clone();
    let on_open_remove = host.on_open_remove.clone();
    let on_close_actions = host.on_close_actions.clone();
    let close_on_change = on_close_actions.clone();
    let on_disconnect = host.on_disconnect.clone();
    let detail_open = computed(move || host.details.get().is_some());
    sheet()
        .open(move || Some(actions_id.with(|id| id.is_some()) && !detail_open.get()))
        .close_on_swipe(true)
        .grabber(true)
        .grabber_style(|style| {
            style
                .self_center()
                .w(px(theme::SHEET_GRABBER_WIDTH))
                .h(px(theme::SHEET_GRABBER_HEIGHT))
                .mt(px(theme::SPACING_SM))
                .mb(px(theme::SPACING_XS))
                .rounded(px(theme::SHEET_GRABBER_HEIGHT / 2.0))
                .bg(theme::muted_foreground().opacity(0.55))
        })
        .backdrop_style(|style| {
            style.bg(nsh_core::color::black().opacity(theme::SHEET_BACKDROP_OPACITY))
        })
        .popup_style(|style| {
            style
                .rounded_t(px(theme::RADIUS_SHEET))
                .border_t_1()
                .border_color(|| theme::border_adaptive())
                .bg(|| theme::card_adaptive())
        })
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event
                && !next
            {
                close_on_change();
            }
        })
        .child(
            view()
                .w_full()
                .flex_col()
                .p(px(theme::SPACING_SM))
                .pb_safe_area_plus(px(theme::SPACING_SM))
                .child(
                    text(move || {
                        actions_id
                            .with(|id| {
                                id.as_ref().and_then(|id| {
                                    saved.with(|saved| {
                                        saved
                                            .iter()
                                            .find(|runtime| runtime.id == id.as_str())
                                            .map(|runtime| runtime.name.clone())
                                    })
                                })
                            })
                            .unwrap_or_default()
                    })
                    .px(px(theme::SPACING_MD))
                    .py(px(theme::SPACING_SM))
                    .truncate()
                    .text_size(px(theme::font_caption()))
                    .text_color(theme::text_muted),
                )
                .child(
                    view()
                        .px(px(theme::SPACING_MD))
                        .pb(px(theme::SPACING_SM))
                        .child(when(
                            move || is_active(actions_id, active),
                            move || {
                                view()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(theme::SPACING_SM))
                                    .child(
                                        view()
                                            .w(px(6.0))
                                            .h(px(6.0))
                                            .rounded(px(3.0))
                                            .bg(theme::color_token("chart-1")),
                                    )
                                    .child(
                                        text(copy::text("Connected", "已连接", "已連線"))
                                            .text_size(px(theme::font_caption()))
                                            .text_color(theme::text_secondary),
                                    )
                            },
                        )),
                )
                .child(when(move || !is_active(actions_id, active), {
                    let on_pick = on_pick.clone();
                    move || {
                        action_row(
                            icon(include_str!("../../assets/icons/refresh.svg")),
                            copy::text("Switch", "切换", "切換"),
                            false,
                            {
                                let on_pick = on_pick.clone();
                                move || {
                                    if let Some(id) = actions_id.get_untracked() {
                                        on_pick(&id);
                                        actions_id.set(None);
                                    }
                                }
                            },
                        )
                    }
                }))
                .child(when(move || is_active(actions_id, active), {
                    let on_disconnect = on_disconnect.clone();
                    let on_close_actions = on_close_actions.clone();
                    move || {
                        action_row(
                            icon(include_str!("../../assets/icons/plug-zap.svg")),
                            copy::text("Disconnect", "断开连接", "中斷連線"),
                            false,
                            {
                                let on_disconnect = on_disconnect.clone();
                                let on_close_actions = on_close_actions.clone();
                                move || {
                                    on_disconnect();
                                    on_close_actions();
                                }
                            },
                        )
                    }
                }))
                .child(action_row(
                    icon(include_str!("../../assets/icons/pencil.svg")),
                    copy::text("Rename", "重命名", "重新命名"),
                    false,
                    {
                        let on_open_rename = on_open_rename.clone();
                        move || {
                            if let Some(id) = actions_id.get_untracked() {
                                on_open_rename(id.as_str());
                            }
                        }
                    },
                ))
                .child(action_row(
                    icon(include_str!("../../assets/icons/circle-question-mark.svg")),
                    copy::text("Details", "详情", "詳細資料"),
                    false,
                    {
                        let on_details = on_details.clone();
                        move || {
                            if let Some(id) = actions_id.get_untracked() {
                                on_details(&id);
                                actions_id.set(None);
                            }
                        }
                    },
                ))
                .child(action_row(
                    icon(include_str!("../../assets/icons/trash-2.svg")),
                    copy::text("Remove", "移除", "移除"),
                    true,
                    {
                        let on_open_remove = on_open_remove.clone();
                        move || {
                            if let Some(id) = actions_id.get_untracked() {
                                on_open_remove(id.as_str());
                            }
                        }
                    },
                )),
        )
}

/// The rename dialog, centered over the page as the GPUI client draws it.
fn rename_dialog(
    host: PairingHost,
    actions_id: Signal<Option<SharedString>>,
    open: Signal<bool>,
) -> impl IntoElement {
    let saved = host.saved;
    let draft = host.rename_draft;
    let rename = host.on_rename_saved.clone();
    let close = host.on_close_rename.clone();
    view()
        .absolute()
        .inset(px(0.0))
        .z_index(20)
        .bg(nsh_core::color::black().opacity(0.72))
        .p(px(theme::SPACING_LG))
        .items_center()
        .justify_center()
        .on_press(|| {})
        .child(when(
            move || open.get(),
            move || {
                view()
                    .w_full()
                    .max_w(px(360.0))
                    .rounded(px(theme::RADIUS_CARD))
                    .border_1()
                    .border_color(theme::border_default)
                    .bg(theme::background_card)
                    .p(px(theme::SPACING_LG))
                    .flex_col()
                    .gap(px(theme::SPACING_MD))
                    .child(
                        text(copy::text(
                            "Rename runtime",
                            "重命名运行时",
                            "重新命名執行環境",
                        ))
                        .text_size(px(theme::font_heading()))
                        .text_color(theme::foreground),
                    )
                    .child(
                        text(copy::text(
                            "Leave this empty to use the name from the runtime.",
                            "留空则使用运行时自身的名称。",
                            "留空則使用執行環境本身的名稱。",
                        ))
                        .text_size(px(theme::font_caption()))
                        .text_color(theme::text_muted),
                    )
                    .child(bordered_field(
                        draft,
                        copy::text("Runtime name", "运行时名称", "執行環境名稱"),
                    ))
                    .child(
                        text(move || {
                            actions_id
                                .with(|id| {
                                    id.as_ref().and_then(|id| {
                                        saved.with(|saved| {
                                            saved
                                                .iter()
                                                .find(|runtime| runtime.id == id.as_str())
                                                .map(|runtime| runtime.name.clone())
                                        })
                                    })
                                })
                                .unwrap_or_default()
                        })
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_muted),
                    )
                    .child(
                        view()
                            .flex_row()
                            .gap(px(theme::SPACING_SM))
                            .child(
                                dialog_secondary(
                                    copy::text("Cancel", "取消", "取消"),
                                    icon(include_str!("../../assets/icons/x.svg")),
                                    {
                                        let close = close.clone();
                                        move || close()
                                    },
                                )
                                .flex_1(),
                            )
                            .child({
                                let rename = rename.clone();
                                let close = close.clone();
                                dialog_primary(
                                    copy::text("Save", "保存", "儲存"),
                                    icon(include_str!("../../assets/icons/check.svg")),
                                    move || {
                                        if let Some(id) = actions_id.get_untracked() {
                                            rename(id.as_str(), &draft.get_untracked());
                                            close();
                                        }
                                    },
                                )
                                .flex_1()
                            }),
                    )
            },
        ))
}

/// The remove confirmation, centered over the page.
fn remove_dialog(
    host: PairingHost,
    actions_id: Signal<Option<SharedString>>,
    open: Signal<bool>,
) -> impl IntoElement {
    let saved = host.saved;
    let remove = host.on_remove_saved.clone();
    let close = host.on_close_remove.clone();
    view()
        .absolute()
        .inset(px(0.0))
        .z_index(20)
        .bg(nsh_core::color::black().opacity(0.72))
        .p(px(theme::SPACING_LG))
        .items_center()
        .justify_center()
        .on_press(|| {})
        .child(when(move || open.get(), {
            let remove = remove.clone();
            move || {
                view()
                    .w_full()
                    .max_w(px(360.0))
                    .rounded(px(theme::RADIUS_CARD))
                    .border_1()
                    .border_color(theme::border_default)
                    .bg(theme::background_card)
                    .p(px(theme::SPACING_LG))
                    .flex_col()
                    .gap(px(theme::SPACING_MD))
                    .child(
                        text(move || {
                            let name = actions_id
                                .with(|id| {
                                    id.as_ref().and_then(|id| {
                                        saved.with(|saved| {
                                            saved
                                                .iter()
                                                .find(|runtime| runtime.id == id.as_str())
                                                .map(|runtime| runtime.name.clone())
                                        })
                                    })
                                })
                                .unwrap_or_default();
                            match copy::language() {
                                crate::copy::Language::English => format!("Remove \"{name}\"?"),
                                crate::copy::Language::SimplifiedChinese => {
                                    format!("移除“{name}”？")
                                }
                                crate::copy::Language::TraditionalChinese => {
                                    format!("移除「{name}」？")
                                }
                                _ => format!("Remove \"{name}\"?"),
                            }
                        })
                        .text_size(px(theme::font_heading()))
                        .text_color(theme::foreground),
                    )
                    .child(
                        text(copy::text(
                            "Deletes the credential stored on this phone. The device authorization on the server is not revoked by this; revoke it there.",
                            "只删除此手机上保存的凭据；服务器上的设备授权不会因此撤销，需要到服务器上撤销。",
                            "只刪除此手機上儲存的認證；伺服器上的裝置授權不會因此撤銷，需要到伺服器上撤銷。",
                        ))
                        .text_size(px(theme::font_caption()))
                        .text_color(theme::text_muted),
                    )
                    .child(
                        view()
                            .flex_row()
                            .justify_end()
                            .gap(px(theme::SPACING_SM))
                            .child(
                                dialog_secondary(
                                    copy::text("Cancel", "取消", "取消"),
                                    icon(include_str!("../../assets/icons/x.svg")),
                                    {
                                        let close = close.clone();
                                        move || close()
                                    },
                                ),
                            )
                            .child({
                                let remove = remove.clone();
                                let close = close.clone();
                                dialog_destructive(
                                    copy::text("Remove", "移除", "移除"),
                                    icon(include_str!("../../assets/icons/trash-2.svg")),
                                    move || {
                                        if let Some(id) = actions_id.get_untracked() {
                                            remove(id.as_str());
                                            close();
                                        }
                                    },
                                )

                            }),
                    )
            }
        }))
}

/// The read-only runtime details page, over the pairing page.
fn details_page(host: PairingHost) -> impl IntoElement {
    let details = host.details;
    let on_close = host.on_close_details.clone();
    view()
        .absolute()
        .inset(px(0.0))
        .z_index(10)
        .bg(theme::block_page)
        .on_press(|| {})
        .child(when(move || details.get().is_some(), {
            let on_close = on_close.clone();
            move || {
                let close = on_close.clone();
                view()
                    .size_full()
                    .flex_col()
                    .pt_safe_area()
                    .child(
                        view()
                            .h(px(SHEET_HEADER_HEIGHT))
                            .flex_row()
                            .items_center()
                            .border_b_1()
                            .border_b_color(theme::border_subtle)
                            .pl(px(theme::SPACING_XS))
                            .pr(px(theme::SPACING_XS))
                            .gap(px(theme::SPACING_SM))
                            .child({
                                let on_close = close.clone();
                                action::icon_button(
                                    icon(include_str!("../../assets/icons/chevron-left.svg")),
                                    copy::text("Back", "返回", "返回"),
                                    move || on_close(),
                                )
                            })
                            .child(
                                text(copy::text(
                                    "Runtime details",
                                    "运行时详情",
                                    "執行環境詳細資料",
                                ))
                                .flex_1()
                                .text_size(px(theme::font_heading()))
                                .text_color(theme::foreground),
                            )
                            .child({
                                let on_close = close.clone();
                                action::icon_button(
                                    icon(include_str!("../../assets/icons/x.svg")),
                                    copy::text("Close", "关闭", "關閉"),
                                    move || on_close(),
                                )
                            }),
                    )
                    .child(
                        view()
                            .flex_1()
                            .flex_col()
                            .px(px(theme::SPACING_LG))
                            .py(px(theme::SPACING_MD))
                            .child(detail_field(
                                copy::text("Type", "类型", "類型"),
                                move || {
                                    details
                                        .with(|details| {
                                            details
                                                .as_ref()
                                                .map(|details| details.kind.label().to_owned())
                                        })
                                        .unwrap_or_default()
                                },
                                false,
                                false,
                            ))
                            .child(detail_field(
                                copy::text("Address", "地址", "位址"),
                                move || {
                                    details.with(|details| {
                                        details
                                            .as_ref()
                                            .map(|details| details.address.clone())
                                            .unwrap_or_default()
                                    })
                                },
                                false,
                                false,
                            ))
                            .child(detail_field(
                                copy::text("Server ID", "服务器 ID", "伺服器 ID"),
                                move || {
                                    details.with(|details| {
                                        details
                                            .as_ref()
                                            .map(|details| details.server_id.clone())
                                            .unwrap_or_default()
                                    })
                                },
                                false,
                                true,
                            ))
                            .child(detail_field(
                                copy::text("Client identity", "客户端身份", "用戶端身分"),
                                || "vibex-mobile".to_owned(),
                                false,
                                true,
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
                                    detail_field(
                                        copy::text("Device name", "设备名称", "裝置名稱"),
                                        move || {
                                            details.with(|details| {
                                                details
                                                    .as_ref()
                                                    .and_then(|details| details.device_name.clone())
                                                    .unwrap_or_default()
                                            })
                                        },
                                        false,
                                        false,
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
                                    detail_field(
                                        copy::text("Added", "添加时间", "新增時間"),
                                        move || {
                                            details.with(|details| {
                                                details
                                                    .as_ref()
                                                    .and_then(|details| details.added.clone())
                                                    .unwrap_or_default()
                                            })
                                        },
                                        false,
                                        false,
                                    )
                                },
                            ))
                            .child(detail_field(
                                copy::text("Last connected", "最近连接", "最近連線"),
                                move || {
                                    details.with(|details| {
                                        details
                                            .as_ref()
                                            .and_then(|details| details.last_connected.clone())
                                            .unwrap_or_else(|| {
                                                copy::text("Never", "从未", "從未").to_owned()
                                            })
                                    })
                                },
                                false,
                                false,
                            ))
                            .child(detail_field(
                                copy::text("Device authorization", "设备授权", "裝置授權"),
                                || {
                                    copy::text(
                                        "Can be revoked on the server",
                                        "可在服务器上撤销",
                                        "可在伺服器上撤銷",
                                    )
                                    .to_owned()
                                },
                                true,
                                false,
                            )),
                    )
            }
        }))
}

fn detail_field(
    label: &'static str,
    value: impl Fn() -> String + 'static,
    muted: bool,
    monospace: bool,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .items_start()
        .gap(px(theme::SPACING_MD))
        .py(px(theme::SPACING_SM))
        .child(
            view().w(px(96.0)).flex_shrink_0().child(
                text(label)
                    .text_size(px(theme::font_caption()))
                    .text_color(theme::text_muted),
            ),
        )
        .child(view().flex_1().min_w(px(0.0)).child({
            let value = text(value);
            let value = value
                .text_size(px(if muted {
                    theme::font_caption()
                } else {
                    theme::font_body()
                }))
                .text_color(if muted {
                    theme::text_muted
                } else {
                    theme::foreground
                });
            if monospace { value.truncate() } else { value }
        }))
}

/// A sheet header: the sheet's name, an optional state action, and close.
fn sheet_header(
    title: &'static str,
    trailing: impl IntoElement,
    on_close: impl Fn() + 'static,
    busy: Signal<bool>,
) -> impl IntoElement {
    view()
        .h(px(SHEET_HEADER_HEIGHT))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .pl(px(theme::SPACING_MD))
        .pr(px(theme::SPACING_XS))
        .child(
            text(title)
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::font_heading()))
                .text_color(theme::foreground),
        )
        .child(trailing)
        .child(
            button()
                .size(px(theme::TOUCH_TARGET))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .active(|style| style.opacity(0.65))
                .accessibility_label(copy::text("Close", "关闭", "關閉"))
                .on_press(on_close)
                .disabled(move || busy.get())
                .disabled_style(|style| style.opacity(0.4))
                .child(
                    svg(icon(include_str!("../../assets/icons/x.svg")))
                        .w(px(theme::ICON_SM))
                        .h(px(theme::ICON_SM))
                        .text_color(theme::text_muted),
                ),
        )
}

/// A state action in a sheet header: retry after a browse failed.
fn retry_action(on_retry: Action) -> impl IntoElement {
    button()
        .h(px(theme::TOUCH_TARGET))
        .px(px(theme::SPACING_SM))
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACING_XS))
        .active(|style| style.opacity(0.7))
        .on_press(move || on_retry())
        .child(
            svg(icon(include_str!("../../assets/icons/rotate-ccw.svg")))
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .text_color(theme::foreground),
        )
        .child(
            text(copy::text("Try Again", "重试", "重試"))
                .text_size(px(theme::font_detail()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::foreground),
        )
}

/// A dialog's secondary action, at the GPUI client's dialog metrics.
fn dialog_secondary(
    label: &'static str,
    icon: ImageSrc,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    dialog_button(label, icon, false, false, on_press)
}

/// A dialog's primary action.
fn dialog_primary(
    label: &'static str,
    icon: ImageSrc,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    dialog_button(label, icon, true, false, on_press)
}

/// A dialog's destructive action: the same shape, in the error ink.
fn dialog_destructive(
    label: &'static str,
    icon: ImageSrc,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    dialog_button(label, icon, false, true, on_press)
}

fn dialog_button(
    label: &'static str,
    icon: ImageSrc,
    primary: bool,
    destructive: bool,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    let border = if primary {
        theme::foreground
    } else if destructive {
        theme::accent_red
    } else {
        theme::border_default
    };
    let fill = if primary {
        theme::foreground
    } else {
        theme::background_card
    };
    let content = if primary {
        theme::background
    } else if destructive {
        theme::accent_red
    } else {
        theme::text_secondary
    };
    button()
        .on_press(on_press)
        .h(px(theme::TOUCH_TARGET))
        .px(px(theme::SPACING_LG))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(border)
        .bg(fill)
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACING_XS))
        .active(|style| style.opacity(0.85))
        .child(
            svg(icon)
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(content),
        )
        .child(
            text(label)
                .text_size(px(theme::font_body()))
                .text_color(content),
        )
}

/// One row of the runtime actions sheet.
fn action_row(
    icon: ImageSrc,
    label: &'static str,
    danger: bool,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    let color = if danger {
        theme::accent_red
    } else {
        theme::foreground
    };
    let icon_color = if danger {
        theme::accent_red
    } else {
        theme::text_muted
    };
    button()
        .h(px(theme::TOUCH_TARGET))
        .w_full()
        .px(px(theme::SPACING_MD))
        .rounded(px(theme::RADIUS_CONTROL))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .active(|style| style.opacity(0.7))
        .on_press(on_press)
        .child(
            svg(icon)
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(icon_color),
        )
        .child(
            text(label)
                .text_size(px(theme::font_body()))
                .text_color(color),
        )
}

fn nearby_message(message: impl Into<SharedString>, error: bool) -> impl IntoElement {
    view().w_full().items_center().justify_center().child(
        text(message.into())
            .text_size(px(theme::font_detail()))
            .text_color(if error {
                theme::accent_red
            } else {
                theme::text_muted
            }),
    )
}

/// The manual sheet's one call to action, at the GPUI client's body size.
fn connect_button(busy: Signal<bool>, on_press: impl Fn() + 'static) -> ButtonElement {
    button()
        .on_press(on_press)
        .w_full()
        .h(px(theme::TOUCH_TARGET))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::action_primary)
        .bg(theme::action_primary)
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(theme::SPACING_SM))
        .active(|style| style.opacity(0.85))
        .disabled(move || busy.get())
        .disabled_style(|style| style.opacity(0.6))
        .child(
            svg(icon(include_str!("../../assets/icons/plug-zap.svg")))
                .w(px(theme::ICON_SM))
                .h(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(theme::action_primary_label),
        )
        .child(
            text(move || {
                if busy.get() {
                    copy::text("Connecting…", "连接中…", "連線中…")
                } else {
                    copy::text("Connect", "连接", "連線")
                }
            })
            .text_size(px(theme::font_body()))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::action_primary_label),
        )
}

fn pairing_field_label(label: impl Fn() -> &'static str + 'static) -> impl IntoElement {
    text(label)
        .mb(px(theme::SPACING_XS))
        .text_size(px(theme::font_caption()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_muted)
}

fn bordered_field(
    value: Signal<String>,
    placeholder: impl IntoReactive<String>,
) -> impl IntoElement {
    view()
        .w_full()
        .h(px(theme::TOUCH_TARGET))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_default)
        .child(field::text_field_flat(value, placeholder).w_full().h_full())
}

fn is_active(id: Signal<Option<SharedString>>, active: Signal<Option<SharedString>>) -> bool {
    id.with(|id| {
        id.as_ref().is_some_and(|id| {
            active.with(|active| {
                active
                    .as_ref()
                    .is_some_and(|active| active.as_str() == id.as_str())
            })
        })
    })
}

/// Whether the entry is a bare code, which needs the server's address too.
fn needs_address(entry: &str) -> bool {
    let entry = entry.trim();
    !entry.is_empty()
        && !(entry.starts_with("vibex://")
            || entry.contains("#/pair/")
            || entry.contains("#/code/"))
}

/// The wall clock in milliseconds, for the verification code's countdown.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// The six-dot ring the old phone client uses while a runtime is asked.
///
/// The ring is a ring of six dots in a two-by-three cell, with one highlight
/// walking it. The phase comes from a shared eight-frame clock, so every row
/// that is probing moves together.
fn activity_indicator(phase: Signal<u32>, color: Hsla) -> impl IntoElement {
    // Ring order around the cell: left column down, right column up.
    let dots = [
        (0.0_f32, 0.0_f32),
        (0.0, 1.0),
        (0.0, 2.0),
        (1.0, 2.0),
        (1.0, 1.0),
        (1.0, 0.0),
    ];
    let center = 10.0_f32;
    let pitch = 5.2_f32;
    let diameter = 3.2_f32;
    view()
        .w(px(20.0))
        .h(px(20.0))
        .flex_shrink_0()
        .relative()
        .children(
            dots.into_iter()
                .enumerate()
                .map(move |(index, (column, row))| {
                    let x = center + (column - 0.5) * pitch - diameter * 0.5;
                    let y = center + (row - 1.0) * pitch - diameter * 0.5;
                    view()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(diameter))
                        .h(px(diameter))
                        .rounded(px(diameter * 0.5))
                        .bg(color)
                        .opacity(move || {
                            let head = phase.get() as f32 / 8.0 * 6.0;
                            let offset = (index as f32 - head).rem_euclid(6.0);
                            let distance = offset.min(6.0 - offset);
                            let intensity =
                                0.5 + 0.5 * (distance / 6.0 * std::f32::consts::TAU).cos();
                            0.28 + 0.72 * intensity
                        })
                }),
        )
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

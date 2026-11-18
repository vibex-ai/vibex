//! The pairing page: the phone's front door while no runtime is connected.
//!
//! The GPUI client's pairing page (`apps/mobile/src/app.rs`, `render_pairing`)
//! has three ways in — scan the desktop's QR code, find a runtime on the same
//! network, or paste a pairing link — plus the runtimes this phone saved
//! before. The structure, the copy and the spacing are the same here; what
//! changes is who draws them: `nsh` mounts platform views, so the camera, the
//! keyboard, the sheets and the scrolling are the platform's.
//!
//! The screen owns only transient view state (which sheet is open, what is
//! typed). Everything authoritative — the saved runtimes, whether a runtime is
//! reachable, what a scan found — arrives through [`PairingHost`], which the
//! app builds. The callbacks are the seams the transport fills in; today the
//! app logs them.
//!
//! The two lists are snapshots for now, taken when the screen is built. The
//! transport integration moves them behind signals with the rest of the app's
//! state, the way the GPUI client reads its store; the layout below does not
//! depend on how that lands.

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
}

/// A runtime the local network found and the user may pair with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyRuntime {
    /// The discovery key, stable while the runtime stays visible.
    pub key: String,
    /// The name the desktop advertises.
    pub name: String,
}

/// What the page may do, and the state it shows.
///
/// Every callback is the host's: the screen never scans, dials or stores
/// anything itself. The app builds one of these from the transport layer; the
/// migration fills them in screen by screen.
#[derive(Clone)]
pub struct PairingHost {
    /// The runtimes this phone saved, most recent first.
    pub saved: Vec<SavedRuntime>,
    /// The runtimes nearby discovery has found so far.
    pub nearby: Vec<NearbyRuntime>,
    /// Open the camera to scan the desktop's QR code.
    pub on_scan: Rc<dyn Fn()>,
    /// Claim the pasted or typed pairing entry.
    pub on_connect: Rc<dyn Fn(&str)>,
    /// Read the clipboard for a pairing entry.
    pub on_paste: Rc<dyn Fn() -> Option<String>>,
    /// Start discovery for runtimes on the same network.
    pub on_find: Rc<dyn Fn()>,
    /// Connect to a saved runtime.
    pub on_pick_saved: Rc<dyn Fn(&str)>,
    /// Pair with a runtime discovery found.
    pub on_pick_nearby: Rc<dyn Fn(&str)>,
}

/// The pairing page.
pub fn pairing(host: PairingHost) -> impl IntoElement {
    let manual_open = signal(false);
    let nearby_open = signal(false);
    let draft = signal(String::new());

    let has_saved = !host.saved.is_empty();
    let saved_host = host.clone();

    let page = view()
        .w_full()
        .max_w(px(CARD_WIDTH))
        .flex_1()
        .flex_col()
        .child(brand::wordmark())
        .when(has_saved, |page| {
            page.child(saved_runtimes(saved_host.clone()))
        })
        .child(
            view()
                .flex_1()
                .flex_col()
                .justify_center()
                .when(!has_saved, |middle| {
                    middle.child(entries_stack(host.clone(), manual_open, nearby_open))
                }),
        )
        .when(has_saved, |page| {
            page.child(entries_row(host.clone(), manual_open, nearby_open))
        });

    view()
        .flex_1()
        .items_center()
        .bg(theme::background)
        .px(px(theme::SPACING_XL))
        .pt_safe_area_plus(px(PAGE_TOP_PAD))
        .pb_safe_area_plus(px(theme::SPACING_LG))
        .child(page)
        .child(manual_sheet(host.clone(), manual_open, draft))
        .child(nearby_sheet(host, nearby_open))
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
        .child(scan_entry(host).w_full())
        .child(find_entry(nearby_open).w_full())
        .child(link_entry(manual_open).w_full())
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
        .child(scan_entry(host).flex_1())
        .child(find_entry(nearby_open).flex_1())
        .child(link_entry(manual_open).flex_1())
}

/// The primary entry: the camera.
fn scan_entry(host: PairingHost) -> ButtonElement {
    let on_scan = host.on_scan;
    action::primary(
        copy::text("Scan", "扫码", "掃碼"),
        icon(include_str!("../../assets/icons/scan-line.svg")),
        move || on_scan(),
    )
}

/// Find a runtime on the same network.
fn find_entry(nearby_open: Signal<bool>) -> ButtonElement {
    action::secondary(
        copy::text("Find", "查找", "尋找"),
        icon(include_str!("../../assets/icons/wifi-outlined.svg")),
        false,
        move || nearby_open.set(true),
    )
}

/// Paste a pairing link or type a code.
fn link_entry(manual_open: Signal<bool>) -> ButtonElement {
    action::secondary(
        copy::text("Link", "链接", "連結"),
        icon(include_str!("../../assets/icons/clipboard-paste.svg")),
        false,
        move || manual_open.set(true),
    )
}

/// The runtimes this phone saved, above the entries.
fn saved_runtimes(host: PairingHost) -> impl IntoElement {
    view()
        .flex_col()
        .w_full()
        .gap(px(theme::SPACING_SM))
        .pt(px(theme::SPACING_XL))
        .children(
            host.saved
                .into_iter()
                .map(move |runtime| saved_row(runtime, host.on_pick_saved.clone())),
        )
}

/// One saved runtime: a status dot, its name, and the display it runs on.
fn saved_row(runtime: SavedRuntime, on_pick: Rc<dyn Fn(&str)>) -> impl IntoElement {
    let status = runtime.status;
    let id = runtime.id;
    button()
        .on_press(move || on_pick(&id))
        .w_full()
        .min_h(px(RUNTIME_ROW_HEIGHT))
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_dim)
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .pl(px(theme::SPACING_MD))
        .pr(px(theme::SPACING_SM))
        .active(|style| style.opacity(0.85))
        .child(
            view()
                .w(px(8.0))
                .h(px(8.0))
                .flex_shrink_0()
                .rounded(px(4.0))
                .bg(move || status.dot())
                .when(status == RuntimeStatus::Probing, |dot| dot.opacity(0.6)),
        )
        .child(
            text(runtime.name)
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
        )
}

/// The manual entry sheet.
fn manual_sheet(host: PairingHost, open: Signal<bool>, draft: Signal<String>) -> impl IntoElement {
    let paste = host.on_paste.clone();
    let connect = host.on_connect.clone();
    let connect_draft = draft;
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
                                move || {
                                    if let Some(text) = paste() {
                                        draft.set(text);
                                    }
                                },
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
                        copy::text("Connect", "连接", "連線"),
                        icon(include_str!("../../assets/icons/plug-zap.svg")),
                        move || {
                            let entry = connect_draft.get_untracked();
                            let entry = entry.trim();
                            if !entry.is_empty() {
                                connect(entry);
                            }
                        },
                    )
                    .w_full()
                    .disabled(move || connect_draft.with(|entry| entry.trim().is_empty()))
                    .disabled_style(|style| style.opacity(0.4))
                }),
        )
}

/// The nearby discovery sheet.
fn nearby_sheet(host: PairingHost, open: Signal<bool>) -> impl IntoElement {
    let candidates = host.nearby.clone();
    let empty = candidates.is_empty();
    let on_find = host.on_find.clone();
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event {
                open.set(*next);
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
                .when(empty, |sheet| {
                    sheet.child(
                        view().h(px(56.0)).items_center().justify_center().child(
                            text(copy::text("Searching...", "正在搜索…", "正在搜尋…"))
                                .text_size(px(theme::FONT_DETAIL))
                                .text_color(theme::text_muted),
                        ),
                    )
                })
                .children(candidates.into_iter().map({
                    let on_pick = host.on_pick_nearby.clone();
                    move |candidate| nearby_row(candidate, on_pick.clone(), open)
                })),
        )
}

/// One runtime discovery found.
fn nearby_row(
    runtime: NearbyRuntime,
    on_pick: Rc<dyn Fn(&str)>,
    open: Signal<bool>,
) -> impl IntoElement {
    let key = runtime.key;
    button()
        .on_press(move || {
            on_pick(&key);
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
                    text(runtime.name)
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

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

//! The between-runtime page: connecting, or an unreachable runtime.
//!
//! The GPUI client shows this page whenever the phone has a credential but no
//! live connection: a wordmark, a status dot, and — once the connection is
//! lost — the way back (retry, switch, disconnect). The `nsh` client has the
//! same page so a failed connection never leaves the reader on a blank
//! conversation with a red line at the bottom.

use std::rc::Rc;

use nsh_core::{element::ImageSrc, prelude::*};

use crate::{copy, theme, ui::brand};

/// Where the phone stands with the runtime it is trying to reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    /// The transport is being opened, or reopened after a drop.
    Connecting,
    /// The runtime answers.
    Online,
    /// The runtime did not answer; the page offers its ways back.
    Offline,
}

impl ConnectionState {
    /// The colour of the status dot.
    fn dot(self) -> Hsla {
        match self {
            Self::Connecting => theme::color_token("warning"),
            Self::Online => theme::color_token("chart-1"),
            Self::Offline => theme::accent_red(),
        }
    }
}

/// What the connecting page may do, and the state it shows.
#[derive(Clone)]
pub struct ConnectingHost {
    pub state: Signal<ConnectionState>,
    /// Why the last attempt failed, when it did.
    pub error: Signal<Option<SharedString>>,
    pub on_retry: Rc<dyn Fn()>,
    pub on_switch_runtime: Rc<dyn Fn()>,
    pub on_disconnect: Rc<dyn Fn()>,
}

/// The page.
pub fn connecting(host: ConnectingHost) -> impl IntoElement {
    let state = host.state;
    let error = host.error;
    let on_retry = host.on_retry.clone();
    let on_switch = host.on_switch_runtime.clone();
    let on_disconnect = host.on_disconnect.clone();

    view()
        .flex_1()
        .relative()
        .items_center()
        .justify_center()
        .bg(theme::background)
        .px(px(theme::SPACING_XL))
        .pt_safe_area()
        .pb_safe_area()
        .gap(px(theme::SPACING_MD))
        .child(brand::wordmark())
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    view()
                        .w(px(8.0))
                        .h(px(8.0))
                        .rounded(px(4.0))
                        .bg(move || state.get().dot()),
                )
                .child(
                    text(move || match state.get() {
                        ConnectionState::Connecting => copy::text(
                            "Connecting to desktop...",
                            "正在连接桌面端…",
                            "正在連線桌面版…",
                        ),
                        ConnectionState::Online => copy::text("Connected", "已连接", "已連線"),
                        ConnectionState::Offline => {
                            copy::text("Desktop is unavailable", "桌面端不可用", "桌面版無法使用")
                        }
                    })
                    .text_size(px(theme::font_detail()))
                    .text_color(theme::text_secondary),
                ),
        )
        .child(when(
            move || error.get().is_some(),
            move || {
                text(move || error.get().unwrap_or_default())
                    .max_w(px(320.0))
                    .text_size(px(theme::font_caption()))
                    .text_color(theme::accent_red)
            },
        ))
        .child(
            view()
                .w(px(300.0))
                .flex_col()
                .gap(px(theme::SPACING_SM))
                .pt(px(theme::SPACING_SM))
                .child(when(move || state.get() != ConnectionState::Connecting, {
                    let on_retry = on_retry.clone();
                    move || {
                        let on_retry = on_retry.clone();
                        crate::ui::action::primary(
                            copy::text("Retry", "重试", "重試"),
                            icon(include_str!("../../assets/icons/refresh.svg")),
                            move || on_retry(),
                        )
                        .w_full()
                    }
                }))
                .child({
                    let on_switch = on_switch.clone();
                    crate::ui::action::secondary(
                        copy::text("Switch runtime", "切换运行时", "切換執行環境"),
                        icon(include_str!("../../assets/icons/refresh.svg")),
                        false,
                        move || on_switch(),
                    )
                    .w_full()
                })
                .child({
                    let on_disconnect = on_disconnect.clone();
                    crate::ui::action::secondary(
                        copy::text("Disconnect", "断开连接", "中斷連線"),
                        icon(include_str!("../../assets/icons/plug-zap.svg")),
                        false,
                        move || on_disconnect(),
                    )
                    .w_full()
                }),
        )
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

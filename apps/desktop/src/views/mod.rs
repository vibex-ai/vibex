use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, div, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};

use crate::locale::Strings;

pub fn session_sidebar(strings: Strings, cx: &App) -> AnyElement {
    v_flex()
        .id("foundation-sidebar")
        .size_full()
        .min_w_0()
        .bg(cx.theme().sidebar)
        .child(
            h_flex()
                .h(px(42.0))
                .flex_none()
                .items_center()
                .gap_2()
                .px_3()
                .border_b_1()
                .border_color(cx.theme().sidebar_border)
                .child(Icon::new(IconName::Bot).small())
                .child(div().text_sm().font_semibold().child(strings.sessions)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .px_4()
                .text_center()
                .child(div().text_sm().font_medium().child(strings.no_workspace))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(strings.open_workspace),
                ),
        )
        .into_any_element()
}

pub fn central_workbench(strings: Strings, runtime_ready: bool, cx: &App) -> AnyElement {
    v_flex()
        .id("foundation-workbench")
        .size_full()
        .min_w_0()
        .bg(cx.theme().background)
        .child(
            h_flex()
                .h(px(42.0))
                .flex_none()
                .items_center()
                .justify_between()
                .px_4()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(IconName::LayoutDashboard).small())
                        .child(div().text_sm().font_semibold().child(strings.workbench)),
                )
                .child(runtime_badge(strings, runtime_ready, cx)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_3()
                .p_6()
                .child(div().text_lg().font_semibold().child(strings.no_workspace))
                .child(
                    div()
                        .max_w(px(440.0))
                        .text_center()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(strings.open_workspace),
                ),
        )
        .child(
            h_flex()
                .h(px(30.0))
                .flex_none()
                .items_center()
                .justify_between()
                .px_3()
                .border_t_1()
                .border_color(cx.theme().border)
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Preview")
                .child(format!(
                    "{} / {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )),
        )
        .into_any_element()
}

pub fn preview_panel(strings: Strings, cx: &App) -> AnyElement {
    placeholder_panel(
        "foundation-preview",
        strings.preview,
        IconName::PanelRight,
        strings.no_preview,
        cx,
    )
}

pub fn right_rail(strings: Strings, cx: &App) -> AnyElement {
    placeholder_panel(
        "foundation-right-rail",
        strings.files_git,
        IconName::FolderOpen,
        strings.no_workspace_files,
        cx,
    )
}

fn placeholder_panel(
    id: &'static str,
    title: &'static str,
    icon: IconName,
    empty: &'static str,
    cx: &App,
) -> AnyElement {
    v_flex()
        .id(id)
        .size_full()
        .min_w_0()
        .bg(cx.theme().background)
        .child(
            h_flex()
                .h(px(42.0))
                .flex_none()
                .items_center()
                .gap_2()
                .px_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(Icon::new(icon).small())
                .child(div().text_sm().font_semibold().child(title)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .px_4()
                .child(
                    div()
                        .min_w_0()
                        .text_center()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(empty),
                ),
        )
        .into_any_element()
}

pub fn runtime_badge(strings: Strings, ready: bool, cx: &App) -> AnyElement {
    h_flex()
        .gap_2()
        .items_center()
        .child(div().size(px(7.0)).rounded_full().bg(if ready {
            cx.theme().success
        } else {
            cx.theme().warning
        }))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(if ready {
                    strings.runtime_ready
                } else {
                    strings.loading_runtime
                }),
        )
        .into_any_element()
}

//! The session's delegation team: tasks the Agent started below it.
//!
//! The runtime owns the tasks (`team_snapshot`); this surface lists what is
//! running or waiting for review and sends the one action a task allows.

use std::rc::Rc;

use nsh_base::{SheetEvent, sheet};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::{copy, theme, ui::action};

/// One task row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamTaskView {
    pub id: String,
    pub title: String,
    /// "Queued", "Starting", "Active", "Awaiting review", "Cancelling",
    /// "Completed", "Failed" or "Cancelled".
    pub phase: String,
    pub detail: String,
    pub can_accept: bool,
    pub can_cancel: bool,
    pub can_interrupt: bool,
}

/// What the team sheet can do.
#[derive(Clone)]
pub struct TeamHost {
    pub open: Signal<bool>,
    pub tasks: Signal<Vec<TeamTaskView>>,
    pub busy: Signal<bool>,
    pub error: Signal<Option<SharedString>>,
    pub on_refresh: Rc<dyn Fn()>,
    pub on_accept: Rc<dyn Fn(&str)>,
    pub on_cancel: Rc<dyn Fn(&str)>,
    pub on_interrupt: Rc<dyn Fn(&str)>,
    pub on_close: Rc<dyn Fn()>,
}

pub fn team(host: TeamHost) -> impl IntoElement {
    let open = host.open;
    let tasks = host.tasks;
    let busy = host.busy;
    let error = host.error;
    sheet()
        .side(Placement::Bottom)
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
                .h(px(520.0))
                .flex_col()
                .bg(theme::background_card)
                .rounded_t(px(16.0))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .child(
                    view()
                        .w_full()
                        .h(px(52.0))
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_XS))
                        .px(px(theme::SPACING_MD))
                        .border_b_1()
                        .border_b_color(theme::border_subtle)
                        .child(
                            text(copy::text("Team", "团队", "團隊"))
                                .flex_1()
                                .text_size(px(theme::font_heading()))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::foreground),
                        )
                        .child({
                            let on_refresh = host.on_refresh.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/refresh.svg")),
                                copy::text("Refresh", "刷新", "重新整理"),
                                move || on_refresh(),
                            )
                            .disabled(move || busy.get())
                            .disabled_style(|style| style.opacity(0.35))
                        })
                        .child({
                            let on_close = host.on_close.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/x.svg")),
                                copy::text("Close", "关闭", "關閉"),
                                move || on_close(),
                            )
                        }),
                )
                .child(when(
                    move || error.get().is_some(),
                    move || {
                        text(move || error.get().unwrap_or_default())
                            .px(px(theme::SPACING_LG))
                            .py(px(theme::SPACING_SM))
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::accent_red)
                    },
                ))
                .child(when(
                    move || tasks.with(|tasks| tasks.is_empty()) && !busy.get(),
                    || {
                        text(copy::text(
                            "No delegated tasks yet.",
                            "还没有委派任务。",
                            "還沒有委派任務。",
                        ))
                        .px(px(theme::SPACING_LG))
                        .py(px(theme::SPACING_LG))
                        .text_size(px(theme::font_caption()))
                        .text_color(theme::text_muted)
                    },
                ))
                .child(
                    scroll().flex_1().child(
                        view()
                            .flex_col()
                            .gap(px(theme::SPACING_SM))
                            .p(px(theme::SPACING_MD))
                            .child(each(
                                move || tasks.get(),
                                |task| task.id.clone(),
                                move |task, _| {
                                    task_row(
                                        task,
                                        busy,
                                        host.on_accept.clone(),
                                        host.on_cancel.clone(),
                                        host.on_interrupt.clone(),
                                    )
                                },
                            )),
                    ),
                ),
        )
}

fn task_row(
    task: ReadSignal<TeamTaskView>,
    busy: Signal<bool>,
    on_accept: Rc<dyn Fn(&str)>,
    on_cancel: Rc<dyn Fn(&str)>,
    on_interrupt: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    let id = task.with(|task| task.id.clone());
    view()
        .w_full()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::block_page)
        .p(px(theme::SPACING_MD))
        .gap(px(theme::SPACING_XS))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    text(move || task.with(|task| task.title.clone()))
                        .flex_1()
                        .truncate()
                        .text_size(px(theme::font_detail()))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::foreground),
                )
                .child(
                    text(move || task.with(|task| task.phase.clone()))
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_muted),
                ),
        )
        .child(
            text(move || task.with(|task| task.detail.clone()))
                .text_size(px(theme::font_caption()))
                .text_color(theme::text_secondary),
        )
        .child(
            view()
                .flex_row()
                .gap(px(theme::SPACING_XS))
                .child(when(move || task.with(|task| task.can_accept), {
                    let on_accept = on_accept.clone();
                    let id = id.clone();
                    move || {
                        let on_accept = on_accept.clone();
                        let id = id.clone();
                        action::secondary(
                            copy::text("Accept", "接受", "接受"),
                            icon(include_str!("../../assets/icons/check.svg")),
                            false,
                            move || on_accept(&id),
                        )
                    }
                }))
                .child(when(move || task.with(|task| task.can_interrupt), {
                    let on_interrupt = on_interrupt.clone();
                    let id = id.clone();
                    move || {
                        let on_interrupt = on_interrupt.clone();
                        let id = id.clone();
                        action::secondary(
                            copy::text("Interrupt", "中断", "中斷"),
                            icon(include_str!("../../assets/icons/x.svg")),
                            false,
                            move || on_interrupt(&id),
                        )
                    }
                }))
                .child(when(move || task.with(|task| task.can_cancel), {
                    let on_cancel = on_cancel.clone();
                    let id = id.clone();
                    move || {
                        let on_cancel = on_cancel.clone();
                        let id = id.clone();
                        action::secondary(
                            copy::text("Cancel", "取消", "取消"),
                            icon(include_str!("../../assets/icons/x.svg")),
                            false,
                            move || on_cancel(&id),
                        )
                    }
                })),
        )
        .child(when(
            move || busy.get(),
            || {
                text(copy::text("Working…", "处理中…", "處理中…"))
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted)
            },
        ))
}

fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

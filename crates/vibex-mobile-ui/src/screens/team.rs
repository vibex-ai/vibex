//! The session's delegation team: tasks the Agent started below it.
//!
//! The runtime owns the tasks (`team_snapshot`); this surface lists what is
//! running or waiting for review and sends the one action a task allows.

use std::rc::Rc;

use nsh_core::prelude::*;

use crate::{
    copy, theme,
    ui::{
        action,
        sheet::{bottom_sheet, sheet_body, sheet_card, sheet_header},
        surface,
    },
};

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
    let on_close = host.on_close.clone();
    let on_refresh = host.on_refresh.clone();
    bottom_sheet(
        move || open.get(),
        move || open.set(false),
        view()
            .w_full()
            .max_h(move || px(window().height.0 * 0.8))
            .min_h(px(0.0))
            .child(sheet_header(
                copy::text("Team", "团队", "團隊"),
                move || {
                    open.set(false);
                    on_close();
                },
            ))
            // How the list stands, and the way to read it again.
            .child(
                view()
                    .w_full()
                    .flex_shrink_0()
                    .flex_row()
                    .items_center()
                    .pl(px(theme::SPACING_XL))
                    .pr(px(theme::SPACING_MD))
                    .child(
                        text(move || {
                            if let Some(error) = error.get() {
                                error
                            } else if busy.get() {
                                copy::text("Working…", "处理中…", "處理中…").into()
                            } else if tasks.with(Vec::is_empty) {
                                copy::text(
                                    "No delegated tasks yet.",
                                    "还没有委派任务。",
                                    "還沒有委派任務。",
                                )
                                .into()
                            } else {
                                copy::text(
                                    "Tasks the Agent handed on",
                                    "Agent 委派出去的任务",
                                    "Agent 委派出去的任務",
                                )
                                .into()
                            }
                        })
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(px(theme::font_caption()))
                        .text_color(move || {
                            if error.with(Option::is_some) {
                                theme::accent_red()
                            } else {
                                theme::text_muted()
                            }
                        }),
                    )
                    .child(
                        action::link(copy::text("Refresh", "刷新", "重新整理"), move || {
                            on_refresh()
                        })
                        .disabled(move || busy.get()),
                    ),
            )
            .child(
                scroll()
                    .flex_shrink(1.0)
                    .min_h(px(0.0))
                    .child(sheet_body().child(each(
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
                    ))),
            ),
    )
}

/// One task: what it is and how it stands, what it reported, and the actions
/// its phase allows.
fn task_row(
    task: ReadSignal<TeamTaskView>,
    busy: Signal<bool>,
    on_accept: Rc<dyn Fn(&str)>,
    on_cancel: Rc<dyn Fn(&str)>,
    on_interrupt: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    let id = task.with(|task| task.id.clone());
    let command = move |label: &'static str, primary: bool, run: Rc<dyn Fn(&str)>| {
        let id = id.clone();
        let button = if primary {
            action::solid(label, move || run(&id))
        } else {
            action::tonal(label, move || run(&id))
        };
        button
            .h(px(theme::SEGMENT_HEIGHT))
            .disabled(move || busy.get())
    };
    sheet_card()
        .p(px(surface::ROW_INSET))
        .gap(px(theme::SPACING_SM))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    text(move || task.with(|task| task.title.clone()))
                        .flex_1()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(theme::font_detail()))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::foreground),
                )
                .child(surface::badge(
                    move || SharedString::from(task.with(|task| task.phase.clone())),
                    theme::text_secondary,
                )),
        )
        .child(when(
            move || task.with(|task| !task.detail.is_empty()),
            move || {
                text(move || task.with(|task| task.detail.clone()))
                    .text_size(px(theme::font_caption()))
                    .line_height(theme::line_height_for(theme::font_caption()))
                    .text_color(theme::text_secondary)
            },
        ))
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(px(theme::SPACING_SM))
                .child(when(move || task.with(|task| task.can_accept), {
                    let command = command.clone();
                    let on_accept = on_accept.clone();
                    move || {
                        command(
                            copy::text("Accept", "接受", "接受"),
                            true,
                            on_accept.clone(),
                        )
                    }
                }))
                .child(when(move || task.with(|task| task.can_interrupt), {
                    let command = command.clone();
                    let on_interrupt = on_interrupt.clone();
                    move || {
                        command(
                            copy::text("Interrupt", "中断", "中斷"),
                            false,
                            on_interrupt.clone(),
                        )
                    }
                }))
                .child(when(move || task.with(|task| task.can_cancel), {
                    let command = command.clone();
                    let on_cancel = on_cancel.clone();
                    move || {
                        command(
                            copy::text("Cancel", "取消", "取消"),
                            false,
                            on_cancel.clone(),
                        )
                    }
                })),
        )
}

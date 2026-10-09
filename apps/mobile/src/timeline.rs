//! Compact presentation of the desktop timeline contract.

use super::*;
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _};
use vibex_ui::timeline::{
    Activity, ActivityKind, ActivitySummary, group_open, is_running, turn_finished,
};
use vibex_ui::tool_detail;

#[cfg(test)]
#[path = "timeline_tests.rs"]
mod tests;

const HEADER_HEIGHT_REM: f32 = 3.0;

impl MobileApp {
    fn row_expansion(&self, id: &str) -> Option<bool> {
        if self.collapsed_timeline_rows.contains(id) {
            Some(false)
        } else if self.expanded_timeline_rows.contains(id) {
            Some(true)
        } else {
            None
        }
    }

    fn set_row_expanded(&mut self, id: String, expanded: bool, cx: &mut Context<Self>) {
        if expanded {
            self.collapsed_timeline_rows.remove(&id);
            self.expanded_timeline_rows.insert(id);
        } else {
            self.expanded_timeline_rows.remove(&id);
            self.collapsed_timeline_rows.insert(id);
        }
        self.timeline_list.remeasure();
        cx.notify();
    }

    fn row_turn_live(&self, row: &TimelineRow) -> bool {
        self.controller
            .as_ref()
            .is_some_and(|controller| controller.state.is_turn_live())
            && self.timeline_turns.last().is_some_and(|turn| {
                row.turn_id.as_deref() == Some(turn.id.as_str()) && !turn_finished(turn)
            })
    }

    pub(super) fn reasoning_live(&self, row: &TimelineRow) -> bool {
        self.row_turn_live(row)
            && row.streaming
            && self.timeline_turns.last().is_some_and(|turn| {
                turn.conclusion_row
                    .as_ref()
                    .is_none_or(|row| row.body.trim().is_empty())
                    && (row.id.starts_with("reasoning-live:")
                        || turn
                            .process_rows
                            .last()
                            .is_some_and(|last| last.id == row.id))
            })
    }

    pub(super) fn render_turn_process_header(
        &self,
        turn: &TimelineConversationTurn,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let id = turn.id.clone();
        let label = format!(
            "{} {}",
            locale::text("Worked for", "工作了", "工作了"),
            format_compact_duration(
                turn.started_at_ms,
                turn_finished(turn).then_some(turn.ended_at_ms).flatten()
            )
        );
        disclosure_button(format!("turn-process:{id}"), label.clone(), expanded, cx)
            .child(disclosure_icon(expanded))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_sm()
                    .font_normal()
                    .child(label),
            )
            .on_click(
                cx.listener(move |this, _, _, cx| this.toggle_process(id.clone(), expanded, cx)),
            )
            .into_any_element()
    }

    pub(super) fn render_reasoning_row(
        &self,
        row: &TimelineRow,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if row.body.trim().is_empty() {
            return div().id(row.id.clone()).into_any_element();
        }
        let live = self.reasoning_live(row);
        // Nothing opens itself: the reader's own choice wins, and without one
        // the shared "expand reasoning by default" preference decides — for a
        // settled thought and a thought the Agent is still on alike. A live
        // body is still drawn in the window once it is open.
        let expanded = self.row_expansion(&row.id).unwrap_or(
            self.effective_timeline_display_settings()
                .reasoning_expanded_by_default,
        );
        // The window is the shape of an open live body, not an invitation: the
        // reader's "show all" choice is what widens it to the whole thought.
        let windowed = live && !self.full_reasoning_rows.contains(&row.id);
        let summary = reasoning_summary_cached(&row.id, row.last_sequence.max(0), &row.body);
        let label = if expanded {
            locale::text("Reasoning", "推理", "推理").to_string()
        } else {
            summary.preview
        };
        let id = row.id.clone();
        let full_id = row.id.clone();
        let mut content = div()
            .id(format!("reasoning:{}", row.id))
            .debug_selector(|| format!("reasoning:{}", row.id))
            .w_full()
            .min_w_0()
            .flex_none()
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        disclosure_button(
                            format!("reasoning-header:{}", row.id),
                            label.clone(),
                            expanded,
                            cx,
                        )
                        .flex_1()
                        .child(activity_icon("icons/brain.svg", theme::text_muted()))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_sm()
                                .font_normal()
                                .child(activity_text(
                                    &format!("reasoning-label:{}", row.id),
                                    &label,
                                    live,
                                )),
                        )
                        .child(disclosure_icon(expanded))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_row_expanded(id.clone(), !expanded, cx)
                        })),
                    )
                    .when(live && expanded, |header| {
                        header.child(
                            Button::new(format!("reasoning-full:{}", row.id))
                                .debug_selector(|| format!("reasoning-full:{}", row.id))
                                .small()
                                .ghost()
                                .h(gpui::rems(HEADER_HEIGHT_REM))
                                .label(if windowed {
                                    locale::text("Show all", "显示全部", "顯示全部")
                                } else {
                                    locale::text("Live window", "实时窗口", "即時視窗")
                                })
                                .accessibility_label(if windowed {
                                    locale::text(
                                        "Show full reasoning",
                                        "显示完整推理",
                                        "顯示完整推理",
                                    )
                                } else {
                                    locale::text(
                                        "Follow latest reasoning",
                                        "跟随最新推理",
                                        "跟隨最新推理",
                                    )
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if windowed {
                                        this.full_reasoning_rows.insert(full_id.clone());
                                    } else {
                                        this.full_reasoning_rows.remove(&full_id);
                                    }
                                    this.collapsed_timeline_rows.remove(&full_id);
                                    this.timeline_list.remeasure();
                                    cx.notify();
                                })),
                        )
                    }),
            );
        if expanded {
            let body = self.render_markdown_view(format!("thought:{}", row.id), row, cx);
            content = content.child(activity_body(
                div()
                    .w_full()
                    .min_w_0()
                    .when(windowed, |body| {
                        body.debug_selector(|| format!("reasoning-window:{}", row.id))
                            .max_h(gpui::rems(
                                markdown::LINE_HEIGHT_REM
                                    * vibex_ui::timeline::REASONING_WINDOW_LINES as f32,
                            ))
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .justify_end()
                    })
                    .child(
                        div()
                            .debug_selector(|| format!("reasoning-content:{}", row.id))
                            .w_full()
                            .min_w_0()
                            .flex_none()
                            .child(body),
                    ),
            ));
        }
        content.into_any_element()
    }

    pub(super) fn render_process_activity_group(
        &self,
        turn: &TimelineConversationTurn,
        group: &TimelineProcessActivityGroup,
        rows: &[TimelineRow],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut summary = ActivitySummary::default();
        for row in rows {
            summary.record(
                &Activity::project(
                    row,
                    self.timeline_row_latest_payload(row),
                    locale::current(),
                ),
                &row.id,
            );
        }
        let label = summary.label(locale::current());
        let expanded = group_open(turn, group, self.row_expansion(&group.id));
        let running = rows
            .iter()
            .any(|row| is_running(row, self.row_turn_live(row)));
        let id = group.id.clone();
        div()
            .id(format!("activity-group:{}", group.id))
            .debug_selector(|| format!("activity-group:{}", group.id))
            .w_full()
            .min_w_0()
            .flex_none()
            .flex()
            .flex_col()
            .child(
                disclosure_button(
                    format!("activity-group-header:{}", group.id),
                    activity_status_label(&label, running, false, false),
                    expanded,
                    cx,
                )
                .child(disclosure_icon(expanded))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .font_normal()
                        .child(activity_text(
                            &format!("group-label:{}", group.id),
                            &label,
                            running,
                        )),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_row_expanded(id.clone(), !expanded, cx)
                })),
            )
            .when(expanded, |group| {
                group.children(
                    rows.iter()
                        .map(|row| self.render_process_activity_line(row, cx)),
                )
            })
            .into_any_element()
    }

    pub(super) fn render_process_activity_line(
        &self,
        row: &TimelineRow,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let payload = self.timeline_row_latest_payload(row);
        let activity = Activity::project(row, payload, locale::current());
        let running = is_running(row, self.row_turn_live(row)) && !activity.is_failed();
        let expanded = self.timeline_row_expanded(&row.id);
        let color = if activity.is_failed() {
            theme::accent_red()
        } else {
            theme::text_muted()
        };
        let icon = if activity.is_failed() {
            "icons/triangle-alert.svg"
        } else {
            icon_path(activity.icon())
        };
        let id = row.id.clone();
        let target = activity.target().to_string();
        let label = activity_status_label(
            &activity.label(),
            running,
            activity.is_failed(),
            row.pending_permission,
        );
        let mut content = div()
            .id(format!("activity:{}", row.id))
            .debug_selector(|| format!("activity:{}", row.id))
            .w_full()
            .min_w_0()
            .flex_none()
            .flex()
            .flex_col()
            .child(
                disclosure_button(format!("activity-header:{}", row.id), label, expanded, cx)
                    .text_color(color)
                    .child(activity_icon(icon, color))
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .font_normal()
                            .child(activity.action()),
                    )
                    .child(
                        div()
                            .debug_selector(|| format!("activity-target:{}", row.id))
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_normal()
                                    .when(activity.path().is_some(), |target| {
                                        target
                                            .rounded(cx.theme().radius * 0.5)
                                            .bg(theme::bg_card_dim())
                                            .px_1()
                                    })
                                    .child(activity_text(
                                        &format!("activity-label:{}", row.id),
                                        &target,
                                        running,
                                    )),
                            ),
                    )
                    .when(row.pending_permission, |header| {
                        header.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme::accent_yellow())
                                .child(locale::text("Approval", "待确认", "待確認")),
                        )
                    })
                    .child(disclosure_icon(expanded))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_row_expanded(id.clone(), !expanded, cx)
                    })),
            );
        if expanded {
            let workspace = self
                .controller
                .as_ref()
                .and_then(|controller| controller.state.active_session.value.as_ref())
                .map(|session| session.workspace_root.as_str());
            let details = tool_detail::project(row, payload, workspace, locale::current());
            let enhanced_files = self
                .effective_timeline_display_settings()
                .enhanced_file_operation_display;
            let details = details
                .iter()
                .enumerate()
                .filter(|(_, detail)| enhanced_files || !detail.is_file_content())
                .filter_map(|(ix, detail)| self.render_activity_detail(&row.id, ix, detail, cx));
            content = content.child(activity_body(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(details),
            ));
        }
        content.into_any_element()
    }

    fn render_activity_detail(
        &self,
        row_id: &str,
        ix: usize,
        detail: &tool_detail::Detail,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let value = detail.text();
        if value.trim().is_empty() {
            return None;
        }
        let copy = detail.copy_text();
        let label = locale::text("Copy {label}", "复制{label}", "複製{label}")
            .replace("{label}", detail.label());
        let value = if detail.is_metadata() {
            format!("{}: {value}", detail.label())
        } else {
            value
        };
        Some(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_start()
                .gap_1()
                .child(
                    div()
                        .id(format!("activity-detail:{row_id}:{ix}"))
                        .debug_selector(|| format!("activity-detail:{row_id}:{ix}"))
                        .min_w_0()
                        .flex_1()
                        .max_h_48()
                        .overflow_y_scroll()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_sm()
                        .line_height(gpui::relative(1.5))
                        .text_color(theme::text_secondary())
                        .whitespace_normal()
                        .child(value),
                )
                .child(
                    Button::new(format!("activity-copy:{row_id}:{ix}"))
                        .debug_selector(|| format!("activity-copy:{row_id}:{ix}"))
                        .small()
                        .ghost()
                        .h(gpui::rems(HEADER_HEIGHT_REM))
                        .w_12()
                        .icon(gpui_component::IconName::Copy)
                        .accessibility_label(label.clone())
                        .tooltip(label)
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                        }),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_command_execution_card(
        &self,
        row: &TimelineRow,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.render_process_activity_line(row, cx)
    }

    pub(super) fn render_file_operation_card(
        &self,
        row: &TimelineRow,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.render_process_activity_line(row, cx)
    }
}

fn disclosure_button(id: String, label: String, expanded: bool, cx: &App) -> Button {
    Button::new(id.clone())
        .debug_selector(move || id)
        .custom(ButtonCustomVariant::new(cx).foreground(theme::text_muted()))
        .compact()
        .w_full()
        .min_w_0()
        .h(gpui::rems(HEADER_HEIGHT_REM))
        .px_0()
        .justify_start()
        .accessibility_label(label)
        .toggled(expanded)
}

fn activity_icon(path: &'static str, color: Hsla) -> gpui::Div {
    div()
        .w_6()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(svg().path(path).size_3p5().text_color(color))
}

fn disclosure_icon(expanded: bool) -> gpui::Div {
    activity_icon(
        if expanded {
            "icons/chevron-down.svg"
        } else {
            "icons/chevron-right.svg"
        },
        theme::text_muted(),
    )
}

fn activity_body(body: impl IntoElement) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .relative()
        .pb_2()
        .pl_8()
        // A physical hairline; both the icon lane and the text inset use rem.
        .child(
            div()
                .absolute()
                .left_3()
                .top_0()
                .bottom_0()
                .w(px(1.0))
                .bg(theme::border_subtle()),
        )
        .child(body)
}

fn activity_text(id: &str, label: &str, running: bool) -> gpui::AnyElement {
    if running {
        ShimmerText::new(label.to_string())
            .id(id.to_string())
            .duration(TIMELINE_SHIMMER_SWEEP)
            .spread(TIMELINE_SHIMMER_SPREAD)
            .into_any_element()
    } else {
        div().child(label.to_string()).into_any_element()
    }
}

fn activity_status_label(label: &str, running: bool, failed: bool, pending: bool) -> String {
    let status = if failed {
        Some(locale::text("Failed", "失败", "失敗"))
    } else if pending {
        Some(locale::text(
            "Waiting for confirmation",
            "等待确认",
            "等待確認",
        ))
    } else if running {
        Some(locale::text("Running", "运行中", "執行中"))
    } else {
        None
    };
    status.map_or_else(|| label.to_string(), |status| format!("{label} · {status}"))
}

pub(super) fn icon_path(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::Command => "icons/square-terminal.svg",
        ActivityKind::Search => "icons/search.svg",
        ActivityKind::Directory => "icons/folder.svg",
        ActivityKind::FileRead => "icons/file-text.svg",
        ActivityKind::FileEdit => "icons/pencil.svg",
        ActivityKind::FileCreate => "icons/file-plus.svg",
        ActivityKind::FileDelete => "icons/trash-2.svg",
        ActivityKind::Todo => "icons/list-checks.svg",
        ActivityKind::Collaboration => "icons/user.svg",
        ActivityKind::Image => "icons/image.svg",
        ActivityKind::Integration => "icons/plug-zap.svg",
        ActivityKind::Retry => "icons/wifi-outlined.svg",
        ActivityKind::Generic => "icons/zap.svg",
    }
}

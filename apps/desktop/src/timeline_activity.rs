use super::*;

#[cfg(test)]
#[path = "timeline_activity_tests.rs"]
mod tests;

/// All activity columns share these relative metrics, including their first
/// layout estimates. The rail stretches with the detail body below the header.
pub(super) const ROW_HEIGHT_REM: f32 = 2.25;
pub(super) const SUMMARY_HEIGHT_REM: f32 = 1.75;
const ICON_SIZE_REM: f32 = 0.875;
const RAIL_ICON_GAP_REM: f32 = 0.125;
const HOVER_GROUP: &str = "timeline-activity";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Activity {
    action: &'static str,
    target: String,
    path: Option<String>,
    icon: ProcessActivityIcon,
    failed: bool,
}

impl Activity {
    pub(super) fn project(row: &TimelineRow, payload: Option<&TimelinePayload>) -> Self {
        use ProcessActivityIcon as Icon;
        let (icon, action, target, path, failed) = match payload {
            Some(TimelinePayload::ToolCall(tool)) => {
                let icon = generic_tool_activity_icon(&tool.tool_name, &tool.summary);
                let summary = if tool.summary.trim().is_empty() {
                    &tool.tool_name
                } else {
                    &tool.summary
                };
                let path = matches!(
                    icon,
                    Icon::FileRead | Icon::FileEdit | Icon::FileCreate | Icon::FileDelete
                )
                .then_some(tool.raw_extension.as_ref())
                .flatten()
                .filter(|extension| extension.locations.len() == 1)
                .and_then(|extension| extension.locations.first())
                .map(|location| location.uri.clone());
                let target = path
                    .as_deref()
                    .unwrap_or_else(|| activity_target(summary, icon));
                (
                    icon,
                    action_label(icon),
                    target.to_string(),
                    path,
                    tool.status == vibex_core::ToolCallStatus::Failed,
                )
            }
            Some(TimelinePayload::Command(command)) => (
                Icon::Command,
                action_label(Icon::Command),
                command.command.clone(),
                None,
                command.status == vibex_core::CommandStatus::Failed,
            ),
            Some(TimelinePayload::FileOperation(file)) => {
                let (icon, action) = match file.operation {
                    FileOperationKind::Read => (Icon::FileRead, action_label(Icon::FileRead)),
                    FileOperationKind::Write => (Icon::FileCreate, action_label(Icon::FileCreate)),
                    FileOperationKind::Edit => (Icon::FileEdit, action_label(Icon::FileEdit)),
                    FileOperationKind::Delete => (Icon::FileDelete, action_label(Icon::FileDelete)),
                    FileOperationKind::Move => {
                        (Icon::FileEdit, locale::text("Move", "移动", "移動"))
                    }
                };
                (
                    icon,
                    action,
                    file.path.clone(),
                    Some(file.path.clone()),
                    false,
                )
            }
            Some(TimelinePayload::WebSearch(search)) => (
                Icon::Search,
                action_label(Icon::Search),
                search.query.clone(),
                None,
                search.status == vibex_core::ToolCallStatus::Failed,
            ),
            Some(TimelinePayload::TodoUpdate(plan)) => (
                Icon::Todo,
                action_label(Icon::Todo),
                format!(
                    "{}/{}",
                    plan.items
                        .iter()
                        .filter(|step| step.status == PlanStepStatus::Completed)
                        .count(),
                    plan.items.len()
                ),
                None,
                plan.items
                    .iter()
                    .any(|step| step.status == PlanStepStatus::Failed),
            ),
            Some(TimelinePayload::Plan(plan)) => (
                Icon::Todo,
                action_label(Icon::Todo),
                format!(
                    "{}/{}",
                    plan.steps
                        .iter()
                        .filter(|step| step.status == PlanStepStatus::Completed)
                        .count(),
                    plan.steps.len()
                ),
                None,
                plan.steps
                    .iter()
                    .any(|step| step.status == PlanStepStatus::Failed),
            ),
            Some(TimelinePayload::Collaboration(agent)) => (
                Icon::Collaboration,
                action_label(Icon::Collaboration),
                if agent.summary.is_empty() {
                    agent.action.clone()
                } else {
                    agent.summary.clone()
                },
                None,
                agent.status == vibex_core::ToolCallStatus::Failed,
            ),
            Some(TimelinePayload::ImageGeneration(image)) => (
                Icon::Image,
                action_label(Icon::Image),
                image.summary.clone(),
                None,
                image.status == vibex_core::ToolCallStatus::Failed,
            ),
            _ => {
                let icon = match row.kind {
                    TimelineRowKind::Command => Icon::Command,
                    TimelineRowKind::FileOperation => Icon::FileRead,
                    TimelineRowKind::WebSearch => Icon::Search,
                    TimelineRowKind::TodoUpdate | TimelineRowKind::Plan => Icon::Todo,
                    TimelineRowKind::Collaboration => Icon::Collaboration,
                    TimelineRowKind::ImageGeneration => Icon::Image,
                    TimelineRowKind::Retry => Icon::Retry,
                    _ => generic_tool_activity_icon(&row.title, ""),
                };
                (
                    icon,
                    action_label(icon),
                    row.title.clone(),
                    row.file_path.clone(),
                    row.failed,
                )
            }
        };
        Self {
            action,
            target: single_line(&path.as_deref().map_or(target, agent_file_display_name)),
            path,
            icon,
            failed,
        }
    }

    pub(super) fn icon(&self) -> ProcessActivityIcon {
        self.icon
    }
    pub(super) fn is_failed(&self) -> bool {
        self.failed
    }
    pub(super) fn label(&self) -> String {
        format!(
            "{} {}",
            self.action,
            self.path.as_deref().unwrap_or(&self.target)
        )
        .trim_end()
        .to_string()
    }
    pub(super) fn approximate_bytes(&self) -> usize {
        self.target.len() + self.path.as_ref().map_or(0, String::len)
    }
}

fn action_label(icon: ProcessActivityIcon) -> &'static str {
    match icon {
        ProcessActivityIcon::Command => locale::text("Run", "运行", "執行"),
        ProcessActivityIcon::Search => locale::text("Search", "搜索", "搜尋"),
        ProcessActivityIcon::Directory => locale::text("List", "列出", "列出"),
        ProcessActivityIcon::FileRead => locale::text("Read", "读取", "讀取"),
        ProcessActivityIcon::FileEdit => locale::text("Edit", "编辑", "編輯"),
        ProcessActivityIcon::FileCreate => locale::text("Write", "写入", "寫入"),
        ProcessActivityIcon::FileDelete => locale::text("Delete", "删除", "刪除"),
        ProcessActivityIcon::Todo => locale::text("Plan", "计划", "計畫"),
        ProcessActivityIcon::Collaboration => "Agent",
        ProcessActivityIcon::Image => locale::text("Image", "图像", "圖像"),
        ProcessActivityIcon::Integration => "MCP",
        ProcessActivityIcon::Retry => locale::text("Retry", "重试", "重試"),
        ProcessActivityIcon::Generic => locale::text("Tool", "工具", "工具"),
    }
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A summary may already start with its action. Remove only a matching verb;
/// command text and unfamiliar summaries retain their complete visible value.
fn activity_target(summary: &str, icon: ProcessActivityIcon) -> &str {
    let summary = summary.trim();
    let Some((verb, target)) = summary.split_once(char::is_whitespace) else {
        return summary;
    };
    let verbs: &[&str] = match icon {
        ProcessActivityIcon::Command => &["run", "ran", "running", "execute", "executed"],
        ProcessActivityIcon::Search => &["search", "searched", "searching"],
        ProcessActivityIcon::Directory => &["list", "listed", "listing"],
        ProcessActivityIcon::FileRead => &["read", "reading", "view", "viewed"],
        ProcessActivityIcon::FileEdit => &["edit", "edited", "editing", "patch", "patched"],
        ProcessActivityIcon::FileCreate => &["write", "wrote", "writing", "create", "created"],
        ProcessActivityIcon::FileDelete => &["delete", "deleted", "remove", "removed"],
        _ => &[],
    };
    if verbs
        .iter()
        .any(|candidate| verb.eq_ignore_ascii_case(candidate))
    {
        target.trim_start()
    } else {
        summary
    }
}

pub(super) fn group_open(
    turn: &TimelineConversationTurn,
    group: &TimelineProcessActivityGroup,
    explicit: Option<bool>,
) -> bool {
    explicit.unwrap_or_else(|| {
        !timeline_turn_finished(turn)
            && timeline_turn_conclusion_row(turn).is_none()
            && (group.end_row == turn.process_rows.len()
                || turn
                    .process_rows
                    .get(group.start_row..group.end_row)
                    .is_some_and(|rows| {
                        rows.iter()
                            .any(|row| row.streaming || row.pending_permission)
                    }))
    })
}

pub(super) fn is_running(row: &TimelineRow, turn_live: bool) -> bool {
    turn_live && row.streaming && !row.failed && !row.pending_permission
}

/// The same button geometry is used by tool, file and plan disclosures.
pub(super) fn header(
    id: String,
    activity: &Activity,
    running: bool,
    open: Option<bool>,
    highlighted_target: Option<AnyElement>,
    statistics: Option<AnyElement>,
    cx: &App,
) -> Button {
    let running = running && !activity.failed;
    let color = if activity.failed {
        cx.theme().danger
    } else {
        resting_color(cx)
    };
    let hover_color = if activity.failed {
        cx.theme().danger
    } else {
        cx.theme().foreground
    };
    let target = highlighted_target
        .unwrap_or_else(|| activity_text("target-shimmer", &activity.target, running));
    let target = h_flex()
        .id("target")
        .debug_selector(|| format!("activity-target:{id}"))
        .min_w_0()
        .gap_1()
        .text_color(color)
        .group_hover(HOVER_GROUP, |style| style.text_color(hover_color))
        .when(activity.path.is_some(), |this| {
            let kind = vibex_desktop_model::file_icon_descriptor(
                &activity.target,
                vibex_core::FileEntryKind::File,
            )
            .kind;
            this.rounded(cx.theme().radius * 0.5)
                .bg(cx.theme().muted.opacity(0.35))
                .px_1p5()
                .py_0p5()
                .child(
                    div()
                        .id("file-icon")
                        .flex_none()
                        .opacity(0.75)
                        .group_hover(HOVER_GROUP, |style| style.opacity(1.0))
                        .child(crate::assets::file_icon(kind, cx).size_3p5()),
                )
        })
        .child(div().min_w_0().truncate().child(target));
    Button::new(id.clone())
        .small()
        .custom(ButtonCustomVariant::new(cx).foreground(color))
        .compact()
        .w_full()
        .min_w_0()
        .h(gpui::rems(ROW_HEIGHT_REM))
        .px_0()
        .justify_start()
        .text_color(color)
        .accessibility_label(status_label(activity.label(), running, activity.failed))
        .when_some(open, |this, open| this.toggled(open))
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .text_xs()
                .font_normal()
                .child(
                    div()
                        .id("action")
                        .flex_none()
                        .text_color(color)
                        .group_hover(HOVER_GROUP, |style| style.text_color(hover_color))
                        .child(activity_text("action-shimmer", activity.action, running)),
                )
                .child(target)
                .children(statistics)
                .when_some(open, |this, open| {
                    this.child(
                        div()
                            .id("disclosure")
                            .debug_selector(|| format!("activity-disclosure:{id}"))
                            .flex_none()
                            .text_color(color)
                            .opacity(if open { 1.0 } else { 0.7 })
                            .group_hover(HOVER_GROUP, |style| {
                                style.text_color(hover_color).opacity(1.0)
                            })
                            .child(
                                Icon::new(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size_3(),
                            ),
                    )
                }),
        )
}

pub(super) fn target(activity: &Activity) -> &str {
    &activity.target
}

pub(super) fn summary_header(
    id: String,
    label: String,
    open: bool,
    running: bool,
    cx: &App,
) -> Button {
    let color = resting_color(cx);
    Button::new(id)
        .group(HOVER_GROUP)
        .small()
        .custom(ButtonCustomVariant::new(cx).foreground(color))
        .compact()
        .w_full()
        .min_w_0()
        .h(gpui::rems(SUMMARY_HEIGHT_REM))
        .px_0()
        .justify_start()
        .text_color(color)
        .accessibility_label(status_label(label.clone(), running, false))
        .toggled(open)
        .child(
            h_flex()
                .id("summary")
                .min_w_0()
                .w_full()
                .gap_1()
                .text_xs()
                .font_normal()
                .text_color(color)
                .group_hover(HOVER_GROUP, |style| style.text_color(cx.theme().foreground))
                .child(
                    div().w_7().flex_none().flex().justify_center().child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size_3p5(),
                    ),
                )
                .child(div().min_w_0().flex_1().truncate().child(activity_text(
                    "summary-shimmer",
                    &label,
                    running,
                ))),
        )
}

fn activity_text(id: &'static str, text: &str, running: bool) -> AnyElement {
    if running {
        ShimmerText::new(text.to_string())
            .id(id)
            .duration(TIMELINE_SHIMMER_SWEEP)
            .spread(TIMELINE_SHIMMER_SPREAD)
            .into_any_element()
    } else {
        StyledText::new(text.to_string()).into_any_element()
    }
}

fn resting_color(cx: &App) -> Hsla {
    let theme = cx.theme();
    if theme.is_dark() {
        theme.muted_foreground.opacity(0.94)
    } else {
        theme.muted_foreground.blend(theme.foreground.opacity(0.20))
    }
}

fn status_label(label: String, running: bool, failed: bool) -> String {
    let status = if failed {
        Some(locale::text("Failed", "失败", "失敗"))
    } else if running {
        Some(locale::text("Running", "运行中", "執行中"))
    } else {
        None
    };
    match status {
        Some(status) => format!("{label} · {status}"),
        None => label,
    }
}

pub(super) fn row(
    id: &str,
    icon: ProcessActivityIcon,
    failed: bool,
    continues: Option<bool>,
    header: AnyElement,
    detail: Option<AnyElement>,
    cx: &App,
) -> AnyElement {
    let icon_top = (ROW_HEIGHT_REM - ICON_SIZE_REM) * 0.5;
    let rail = div()
        .relative()
        .w_7()
        .flex_none()
        .when(continues.is_some(), |this| {
            this.child(
                rail_line(cx)
                    .debug_selector(|| format!("activity-rail-before:{id}"))
                    .top_0()
                    .h(gpui::rems(icon_top - RAIL_ICON_GAP_REM)),
            )
        })
        .when(continues == Some(true), |this| {
            this.child(
                rail_line(cx)
                    .debug_selector(|| format!("activity-rail-after:{id}"))
                    .top(gpui::rems(icon_top + ICON_SIZE_REM + RAIL_ICON_GAP_REM))
                    .bottom_0(),
            )
        })
        .child(
            div()
                .id("rail-icon")
                .debug_selector(|| format!("activity-rail-icon:{id}"))
                .absolute()
                .top(gpui::rems(icon_top))
                .h(gpui::rems(ICON_SIZE_REM))
                .w_full()
                .flex()
                .justify_center()
                .text_color(if failed {
                    cx.theme().danger
                } else {
                    resting_color(cx)
                })
                .group_hover(HOVER_GROUP, |style| {
                    style.text_color(if failed {
                        cx.theme().danger
                    } else {
                        cx.theme().foreground
                    })
                })
                .child(process_activity_icon(icon).size(gpui::rems(ICON_SIZE_REM))),
        );
    h_flex()
        .id(id.to_string())
        .group(HOVER_GROUP)
        .w_full()
        .min_w_0()
        .flex_none()
        .items_stretch()
        .gap_1()
        .child(rail)
        .child(v_flex().min_w_0().flex_1().child(header).children(detail))
        .into_any_element()
}

fn rail_line(cx: &App) -> Div {
    // Only the separator is a physical hairline; its column and endpoints zoom.
    div().absolute().w_full().flex().justify_center().child(
        div()
            .w(px(1.0))
            .h_full()
            .bg(cx.theme().border.opacity(0.55)),
    )
}

pub(super) fn detail(id: String, label: &str, value: &str, cx: &App) -> AnyElement {
    let copy_value = value.to_string();
    v_flex()
        .min_w_0()
        .w_full()
        .gap_1()
        .child(
            h_flex()
                .min_w_0()
                .gap_2()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label.to_string()),
                )
                .child(
                    Button::new(format!("copy:{id}"))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Copy)
                        .tooltip(locale::text("Copy", "复制", "複製"))
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_value.clone()))
                        }),
                ),
        )
        .child(
            div()
                .id(id)
                .min_w_0()
                .w_full()
                .max_h_40()
                .overflow_y_scrollbar()
                .scroll_gutter()
                .font_family(cx.theme().mono_font_family.clone())
                .font_weight(code_font_weight(cx))
                .text_size(cx.theme().mono_font_size)
                .line_height(relative(1.5))
                .text_color(cx.theme().muted_foreground)
                .child(value.to_string()),
        )
        .into_any_element()
}

#[derive(Default)]
pub(super) struct ActivitySummary {
    commands: usize,
    reads: usize,
    edits: BTreeSet<String>,
    searches: usize,
    plans: usize,
    agents: usize,
    other: usize,
    failed: usize,
}

impl ActivitySummary {
    pub(super) fn record(&mut self, activity: &Activity, row_id: &str) {
        self.failed += usize::from(activity.failed);
        match activity.icon {
            ProcessActivityIcon::Command => self.commands += 1,
            ProcessActivityIcon::FileRead => self.reads += 1,
            ProcessActivityIcon::FileEdit
            | ProcessActivityIcon::FileCreate
            | ProcessActivityIcon::FileDelete => {
                self.edits
                    .insert(activity.path.as_deref().unwrap_or(row_id).to_string());
            }
            ProcessActivityIcon::Search | ProcessActivityIcon::Directory => self.searches += 1,
            ProcessActivityIcon::Todo => self.plans += 1,
            ProcessActivityIcon::Collaboration => self.agents += 1,
            _ => self.other += 1,
        }
    }

    pub(super) fn label(&self) -> String {
        let entries = [
            (
                self.commands,
                "Ran {n} command",
                "Ran {n} commands",
                "运行 {n} 条命令",
                "執行 {n} 條命令",
            ),
            (
                self.reads,
                "Read {n} file",
                "Read {n} files",
                "读取 {n} 个文件",
                "讀取 {n} 個檔案",
            ),
            (
                self.edits.len(),
                "Changed {n} file",
                "Changed {n} files",
                "修改 {n} 个文件",
                "修改 {n} 個檔案",
            ),
            (
                self.searches,
                "Searched {n} time",
                "Searched {n} times",
                "搜索 {n} 次",
                "搜尋 {n} 次",
            ),
            (
                self.plans,
                "Updated {n} plan",
                "Updated {n} plans",
                "更新 {n} 次计划",
                "更新 {n} 次計畫",
            ),
            (
                self.agents,
                "Called {n} agent",
                "Called {n} agents",
                "调用 {n} 个 Agent",
                "呼叫 {n} 個 Agent",
            ),
            (
                self.other,
                "Called {n} tool",
                "Called {n} tools",
                "调用 {n} 次工具",
                "呼叫 {n} 次工具",
            ),
            (
                self.failed,
                "{n} failed",
                "{n} failed",
                "{n} 项失败",
                "{n} 項失敗",
            ),
        ];
        entries
            .into_iter()
            .filter(|(count, ..)| *count > 0)
            .map(|(count, singular, plural, zh, tw)| {
                locale::text(if count == 1 { singular } else { plural }, zh, tw)
                    .replace("{n}", &count.to_string())
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

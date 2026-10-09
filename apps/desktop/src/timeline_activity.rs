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

pub(super) use vibex_ui::timeline::{
    Activity, ActivitySummary, activity_target, group_open, is_running,
};

pub(super) fn current_locale() -> vibex_ui::locale::Locale {
    match locale::current_locale() {
        locale::ResolvedLocale::En => vibex_ui::locale::Locale::En,
        locale::ResolvedLocale::ZhCn => vibex_ui::locale::Locale::ZhCn,
        locale::ResolvedLocale::ZhTw => vibex_ui::locale::Locale::ZhTw,
    }
}

pub(super) fn project(row: &TimelineRow, payload: Option<&TimelinePayload>) -> Activity {
    Activity::project(row, payload, current_locale())
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
    let running = running && !activity.is_failed();
    let color = if activity.is_failed() {
        cx.theme().danger
    } else {
        resting_color(cx)
    };
    let hover_color = if activity.is_failed() {
        cx.theme().danger
    } else {
        cx.theme().foreground
    };
    let target = highlighted_target
        .unwrap_or_else(|| activity_text("target-shimmer", activity.target(), running));
    let target = h_flex()
        .id("target")
        .debug_selector(|| format!("activity-target:{id}"))
        .min_w_0()
        .gap_1()
        .text_color(color)
        .group_hover(HOVER_GROUP, |style| style.text_color(hover_color))
        .when(activity.path().is_some(), |this| {
            let kind = vibex_desktop_model::file_icon_descriptor(
                activity.target(),
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
        .accessibility_label(status_label(
            activity.label(),
            running,
            activity.is_failed(),
        ))
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
                        .child(activity_text("action-shimmer", activity.action(), running)),
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
    activity.target()
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

pub(super) fn detail(
    id: String,
    label: &str,
    value: &str,
    copy_value: &str,
    cx: &App,
) -> AnyElement {
    let copy_value = copy_value.to_string();
    let copy_label = locale::text("Copy {label}", "复制{label}", "複製{label}")
        .replace("{label}", &label.to_lowercase());
    h_flex()
        .debug_selector(|| format!("activity-detail:{id}"))
        .min_w_0()
        .w_full()
        .items_start()
        .gap_2()
        .child(
            div()
                .id(id.clone())
                .debug_selector(|| format!("activity-detail-text:{id}"))
                .min_w_0()
                .flex_1()
                .max_h_40()
                .overflow_y_scrollbar()
                .scroll_gutter()
                .font_family(cx.theme().mono_font_family.clone())
                .font_weight(code_font_weight(cx))
                .text_size(cx.theme().mono_font_size)
                .line_height(relative(1.5))
                .text_color(cx.theme().muted_foreground)
                .child(value.trim_end_matches(['\n', '\r']).to_string()),
        )
        .child(
            Button::new(format!("copy:{id}"))
                .xsmall()
                .ghost()
                .flex_none()
                .icon(IconName::Copy)
                .accessibility_label(copy_label.clone())
                .tooltip(copy_label)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_value.clone()))
                }),
        )
        .into_any_element()
}

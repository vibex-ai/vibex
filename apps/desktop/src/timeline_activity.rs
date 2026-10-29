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

/// Longest gap between two timeline frames that still describes a frame budget.
///
/// A longer gap means the timeline went quiet in between, so the second render
/// says nothing about how fast the window is drawing.
const LIVE_LABEL_IDLE_GAP: Duration = Duration::from_millis(150);
/// Gaps shorter than this belong to a second view drawing inside one frame.
const LIVE_LABEL_SAME_FRAME_GAP: Duration = Duration::from_millis(4);
/// Consecutive late frames before live labels stop asking for the next one.
const LIVE_LABEL_LATE_STREAK: u32 = 2;
/// Consecutive on-time frames before they start asking again.
const LIVE_LABEL_RECOVERED_STREAK: u32 = 2;
/// A frame may land this far past the display's interval and still count as on
/// time, in tenths. Missing the display interval once is the first sign the
/// frame is over budget; a repeating animation would only add a request such a
/// frame cannot answer.
const LIVE_LABEL_LATE_SLACK_TENTHS: u64 = 18;
/// The gap that restores the sweep, in tenths: close enough to the display's
/// interval that a recovered timeline animates again on the next frame.
const LIVE_LABEL_RECOVERED_SLACK_TENTHS: u64 = 13;

/// What the live labels' frame gate decided, and the streaks it decides with.
///
/// The decision is a pure state machine so its hysteresis is testable without
/// a window. Idle gaps never reach it: each gap it sees either keeps the sweep,
/// stops it, or restores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LiveLabelGate {
    animate: bool,
    late_frames: u32,
    on_time_frames: u32,
}

impl LiveLabelGate {
    const ANIMATING: Self = Self {
        animate: true,
        late_frames: 0,
        on_time_frames: 0,
    };

    /// The state after one frame `gap`, measured against the display's own
    /// `refresh` interval.
    ///
    /// The two budgets leave a band between them where a sweep that runs keeps
    /// running and a sweep that stopped stays stopped, so a frame that lands
    /// near the budget cannot flip the animation every other frame.
    fn observe(self, gap: Duration, refresh: Duration) -> Self {
        let refresh = (refresh.as_micros() as u64).max(1);
        let gap = gap.as_micros() as u64;
        if gap > refresh * LIVE_LABEL_LATE_SLACK_TENTHS / 10 {
            let late_frames = self.late_frames.saturating_add(1);
            Self {
                animate: self.animate && late_frames < LIVE_LABEL_LATE_STREAK,
                late_frames,
                on_time_frames: 0,
            }
        } else if gap <= refresh * LIVE_LABEL_RECOVERED_SLACK_TENTHS / 10 {
            let on_time_frames = self.on_time_frames.saturating_add(1);
            Self {
                animate: self.animate || on_time_frames >= LIVE_LABEL_RECOVERED_STREAK,
                late_frames: 0,
                on_time_frames,
            }
        } else {
            Self {
                animate: self.animate,
                late_frames: 0,
                on_time_frames: 0,
            }
        }
    }
}

static LIVE_LABEL_EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static LIVE_LABEL_LAST_FRAME_MICROS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static LIVE_LABEL_GATE: std::sync::Mutex<LiveLabelGate> =
    std::sync::Mutex::new(LiveLabelGate::ANIMATING);

/// Samples the gap since the previous timeline frame and decides whether live
/// labels may keep a repeating animation mounted.
///
/// The sweep is what makes a running row read as alive, and on a frame with
/// headroom it costs nothing extra: that frame was going to be drawn anyway.
/// On a frame that is already late it is the opposite. A repeating
/// `with_animation` asks the window for a frame on every display refresh for as
/// long as it is mounted, so a workbench whose frames overrun the budget never
/// idles — the main thread stays saturated by frames it cannot deliver on time,
/// which is exactly the state an expanded, busy timeline lands in. This gate
/// drops live labels back to static text while the timeline misses its display
/// interval and restores the sweep once it holds it again.
pub(super) fn observe_live_label_frame(window: &Window, cx: &App) {
    let now = Instant::now();
    let epoch = *LIVE_LABEL_EPOCH.get_or_init(|| now);
    let now_micros = now.saturating_duration_since(epoch).as_micros() as u64;
    let previous = LIVE_LABEL_LAST_FRAME_MICROS.swap(now_micros, Ordering::Relaxed);
    let gap = now_micros.saturating_sub(previous);
    // The first frame has nothing to compare against; a second view drawing
    // inside one frame is not a frame of its own; and a gap this wide means the
    // timeline went quiet, which says nothing about the budget.
    if previous == 0
        || gap < LIVE_LABEL_SAME_FRAME_GAP.as_micros() as u64
        || gap > LIVE_LABEL_IDLE_GAP.as_micros() as u64
    {
        return;
    }
    // Panels on a display that reports no interval keep animating: the gate
    // exists to protect a budget, and without one there is nothing to protect.
    let Some(refresh) = window.refresh_interval(cx) else {
        return;
    };
    let mut gate = LIVE_LABEL_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *gate = gate.observe(Duration::from_micros(gap), refresh);
}

/// Whether a live label may mount its repeating sweep this frame.
pub(super) fn live_labels_animate() -> bool {
    LIVE_LABEL_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .animate
}

/// The same button geometry is used by tool, file and plan disclosures.
/// The disclosure chevron, turned by the spring its body travels on.
///
/// Swapping a right chevron for a down one jumps a quarter turn in one frame
/// while the body below it is still easing open; turning one glyph with the
/// body's own progress keeps the two moving together.
fn disclosure_chevron(progress: f32) -> Icon {
    Icon::new(IconName::ChevronRight).rotate(gpui::radians(
        progress.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2,
    ))
}

/// `disclosure` is whether the row is open and how far its body has travelled.
pub(super) fn header(
    id: String,
    activity: &Activity,
    running: bool,
    disclosure: Option<(bool, f32)>,
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
        .when_some(disclosure, |this, (open, _)| this.toggled(open))
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
                .when_some(disclosure, |this, (open, progress)| {
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
                            .child(disclosure_chevron(progress).size_3()),
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
    progress: f32,
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
                    div()
                        .w_7()
                        .flex_none()
                        .flex()
                        .justify_center()
                        .child(disclosure_chevron(progress).size_3p5()),
                )
                .child(div().min_w_0().flex_1().truncate().child(activity_text(
                    "summary-shimmer",
                    &label,
                    running,
                ))),
        )
}

fn activity_text(id: &'static str, text: &str, running: bool) -> AnyElement {
    if running && live_labels_animate() {
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
        .debug_selector(|| format!("activity-row:{id}"))
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

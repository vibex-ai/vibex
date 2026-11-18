use super::*;
use crate::shimmer::ShimmerText;

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
    Activity, ActivitySummary, activity_target, display_command, group_open, is_running,
};

/// How long a settled command or agent call ran, as a muted header label.
pub(super) fn row_duration(
    row: &TimelineRow,
    activity: &Activity,
    items: &[vibex_core::TimelineItem],
    cx: &App,
) -> Option<AnyElement> {
    let elapsed = vibex_ui::timeline::row_duration_ms(row, activity, items)?;
    Some(
        div()
            .flex_none()
            .text_xs()
            .text_color(cx.theme().muted_foreground.opacity(0.72))
            .child(vibex_ui::timeline::format_duration(elapsed))
            .into_any_element(),
    )
}

pub(super) fn current_locale() -> vibex_ui::locale::Locale {
    // The shared projection reads the same catalogue as the rest of the
    // interface, so the bridge must pass the resolved locale through instead of
    // collapsing everything but the three shipped languages to English.
    locale::current_locale().shared()
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
/// The app's repeating-animation cadence.
///
/// `with_max_fps(30)` schedules the next render about 33ms out, so a gate that
/// measured against a 60Hz display interval would read every deliberate 30fps
/// frame as late and switch the sweep off for good. The frame budget is the
/// display interval floored at this value.
const LIVE_LABEL_FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// A frame may land this far past the frame budget and still count as on time,
/// in tenths. Missing the budget once is the first sign the frame is over
/// budget; a repeating animation would only add a request such a frame cannot
/// answer.
const LIVE_LABEL_LATE_SLACK_TENTHS: u64 = 18;
/// The gap that restores the sweep, in tenths: close enough to the frame budget
/// that a recovered timeline animates again on the next frame.
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

impl Default for LiveLabelGate {
    fn default() -> Self {
        Self::ANIMATING
    }
}

impl LiveLabelGate {
    const ANIMATING: Self = Self {
        animate: true,
        late_frames: 0,
        on_time_frames: 0,
    };

    /// The state after one frame `gap`, measured against the frame budget: the
    /// display's `refresh` interval floored at [`LIVE_LABEL_FRAME_INTERVAL`].
    ///
    /// The two budgets leave a band between them where a sweep that runs keeps
    /// running and a sweep that stopped stays stopped, so a frame that lands
    /// near the budget cannot flip the animation every other frame.
    fn observe(self, gap: Duration, refresh: Duration) -> Self {
        let refresh = (refresh.as_micros() as u64).max(1);
        let budget = refresh.max(LIVE_LABEL_FRAME_INTERVAL.as_micros() as u64);
        let gap = gap.as_micros() as u64;
        if gap > budget * LIVE_LABEL_LATE_SLACK_TENTHS / 10 {
            let late_frames = self.late_frames.saturating_add(1);
            Self {
                animate: self.animate && late_frames < LIVE_LABEL_LATE_STREAK,
                late_frames,
                on_time_frames: 0,
            }
        } else if gap <= budget * LIVE_LABEL_RECOVERED_SLACK_TENTHS / 10 {
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

/// A window's frame cadence, and what its gate decided from it.
///
/// This belongs to the workbench, not to the process. A window id is unique
/// only within one `App` — every `App` starts its slotmap at the same key — so a
/// process-wide table keyed by window id made two tests share one entry. The
/// state also needs an epoch to subtract from, and an epoch only means anything
/// against the clock it was taken from: each `App` owns its executor clock, so a
/// shared epoch made every gap a comparison of two unrelated time bases.
///
/// Both failures looked identical from outside — the shimmer probe passed alone
/// and failed inside the suite — because both ended in a stale "stop
/// animating" verdict. Keeping the state on the entity removes the question:
/// one workbench, one clock, one gate.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LiveLabelFrameGate {
    epoch: Option<Instant>,
    last_frame_micros: u64,
    gate: LiveLabelGate,
}

impl LiveLabelFrameGate {
    /// Samples one frame and answers whether this frame's live labels may keep
    /// their sweep.
    ///
    /// The first frame has nothing to compare against; a second view drawing
    /// inside one frame is not a frame of its own; and a gap this wide means the
    /// timeline went quiet, which says nothing about the budget.
    pub(super) fn observe(&mut self, now: Instant, refresh: Option<Duration>) -> bool {
        let epoch = *self.epoch.get_or_insert(now);
        let now_micros = now.saturating_duration_since(epoch).as_micros() as u64;
        let previous = std::mem::replace(&mut self.last_frame_micros, now_micros);
        if previous == 0 {
            return self.gate.animate;
        }
        let gap = now_micros.saturating_sub(previous);
        if gap < LIVE_LABEL_SAME_FRAME_GAP.as_micros() as u64
            || gap > LIVE_LABEL_IDLE_GAP.as_micros() as u64
        {
            return self.gate.animate;
        }
        // Panels on a display that reports no interval keep animating: the gate
        // exists to protect a budget, and without one there is nothing to
        // protect.
        let Some(refresh) = refresh else {
            return self.gate.animate;
        };
        self.gate = self.gate.observe(Duration::from_micros(gap), refresh);
        self.gate.animate
    }

    /// Whether a live label may mount its repeating sweep right now.
    pub(super) fn animating(&self) -> bool {
        self.gate.animate
    }
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
///
/// `live_labels_animate` is the owner's frame-gate answer for this frame. It is
/// passed in rather than read here because the gate belongs to the view that
/// draws, and a row does not know which view that is.
#[allow(clippy::too_many_arguments)]
pub(super) fn header(
    id: String,
    activity: &Activity,
    running: bool,
    disclosure: Option<(bool, f32)>,
    highlighted_target: Option<AnyElement>,
    statistics: Option<AnyElement>,
    live_labels_animate: bool,
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
    let target = highlighted_target.unwrap_or_else(|| {
        activity_text(
            "target-shimmer",
            activity.target(),
            running,
            live_labels_animate,
        )
    });
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
                        .child(activity_text(
                            "action-shimmer",
                            activity.action(),
                            running,
                            live_labels_animate,
                        )),
                )
                .child(target)
                // A failed call says why while it is closed; open, its body
                // shows the whole output and the line would only repeat it.
                .when_some(
                    activity
                        .error_line()
                        .filter(|_| !disclosure.is_some_and(|(open, _)| open)),
                    |this, line| {
                        this.child(
                            div()
                                .id("error-line")
                                .min_w_0()
                                .flex_shrink(1.0)
                                .truncate()
                                .text_color(cx.theme().danger.opacity(0.78))
                                .child(line.to_string()),
                        )
                    },
                )
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
    live_labels_animate: bool,
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
                    live_labels_animate,
                ))),
        )
}

fn activity_text(
    id: &'static str,
    text: &str,
    running: bool,
    live_labels_animate: bool,
) -> AnyElement {
    if running && live_labels_animate {
        ShimmerText::new(text.to_string())
            .id(id)
            .debug_selector(format!("activity-shimmer:{id}"))
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

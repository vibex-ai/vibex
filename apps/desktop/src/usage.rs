use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use chrono::{Datelike as _, NaiveDate};
use gpui::{
    AnimationExt as _, AnyElement, App, Bounds, Context, Edges, ElementId, Entity, Hsla,
    InteractiveElement as _, IntoElement, Pixels, Render, SharedString, Styled as _, Task,
    WeakEntity, Window, div, point, prelude::*, px, size, transparent_black,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Icon, IconName, Selectable as _,
    Sizable as _, Size, StyledExt as _,
    button::{Button, ButtonGroup, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    tooltip::Tooltip,
    v_flex,
};
use vibex_backend::BackendFacade;
use vibex_core::{
    AgentId, AgentUsageAggregate, AgentUsageAnnualProjection, AgentUsageDailyModelUsage,
    AgentUsageDimension, AgentUsageDimensionRow, AgentUsageFilterOption, AgentUsageMetricCoverage,
    AgentUsageMetricValue, AgentUsageRange, AgentUsageSortDirection, AgentUsageSortMetric,
    AgentUsageStatistics, AgentUsageStatisticsRequest, AgentUsageTrendMetric, ProjectId,
    ProviderProfileId, VibexSessionId,
};

use crate::{
    gpui_ext::button_with_aria_label,
    locale, motion, skeleton, theme,
    usage_charts::{
        ModelChart, ModelChartCategory, ModelChartDay, TrendChart, TrendChartBucket,
        TrendChartSeries,
    },
};

const USAGE_CHART_HEIGHT: f32 = 176.0;
const USAGE_HEATMAP_CELL_SIZE: f32 = 12.0;
const USAGE_HEATMAP_GAP: f32 = 3.0;
const USAGE_HEATMAP_MIN_WIDTH: f32 = 840.0;
const USAGE_MODEL_CHART_MIN_WIDTH: f32 = 720.0;
const USAGE_SESSION_FILTER_MENU_WIDTH: f32 = 420.0;
const USAGE_SESSION_FILTER_LABEL_MAX_WIDTH_UNITS: usize = 48;
/// One toolbar control: the range shell's segment plus its inset and hairline.
const USAGE_TOOLBAR_CONTROL_HEIGHT: f32 =
    USAGE_RANGE_SEGMENT_HEIGHT + USAGE_RANGE_INSET * 2.0 + 2.0;
/// One summary tile: `py_3` + a 20px label row + `gap_2` + the `text_xl` value
/// line at `relative(1.2)` + `py_3`. The loading placeholder uses the same
/// figure so the grid does not resize when the numbers land.
const USAGE_SUMMARY_TILE_HEIGHT: f32 = 76.0;
/// Tiles `render_summary` builds. The placeholder grid must stay on the same
/// row count or the page reflows when the statistics arrive.
const USAGE_SUMMARY_METRIC_COUNT: usize = 6;
/// Rows the placeholder stands in for in the dimension table. The real count is
/// only known once the statistics arrive, so the placeholder shows a typical
/// breakdown instead of guessing it.
const USAGE_LOADING_TABLE_ROWS: usize = 5;
const USAGE_MODEL_LIMIT: usize = 10;
const USAGE_OTHER_MODEL_ID: &str = "__vibex_other_models__";
const USAGE_AGENT_DEFAULT_MODEL_ID: &str = "__vibex_agent_default_model__";

/// The time ranges the toolbar offers, in the order the slider lays them out.
/// The segment list and the thumb's slots both read this one order, so the pill
/// always lands on the segment it belongs to.
const USAGE_RANGES: [AgentUsageRange; 4] = [
    AgentUsageRange::Today,
    AgentUsageRange::Last7Days,
    AgentUsageRange::Last30Days,
    AgentUsageRange::AllTime,
];

/// The shell's padding, the inset that keeps the thumb clear of the segment it
/// sits on, and the amount by which the thumb's radius steps down from the
/// shell's so the two curves stay concentric.
const USAGE_RANGE_INSET: f32 = 2.0;

/// Width of the shell's hairline.
const USAGE_RANGE_BORDER: f32 = 1.0;

/// Height of one segment. The shell adds [`USAGE_RANGE_INSET`] and its hairline
/// on both edges, which lands the slider on the height of the toolbar's outline
/// controls beside it.
const USAGE_RANGE_SEGMENT_HEIGHT: f32 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageFilterKind {
    Agent,
    Project,
    ProviderProfile,
    Model,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageContentState {
    Loading,
    Empty,
    Ready,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageTrendView {
    Bars,
    Heatmap,
    Models,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageModelMetric {
    Requests,
    TotalTokens,
}

#[derive(Debug, Clone, Copy)]
struct UsageTrendSeries {
    metric: AgentUsageTrendMetric,
    label: &'static str,
    color: Hsla,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsageHeatmapEntry {
    label: String,
    value: Option<u64>,
}

pub struct UsageView {
    backend: Option<BackendFacade>,
    request: AgentUsageStatisticsRequest,
    statistics: Option<AgentUsageStatistics>,
    loading: bool,
    stale: bool,
    error: Option<(String, String)>,
    trend_view: UsageTrendView,
    enabled_trend_metrics: Vec<AgentUsageTrendMetric>,
    model_metric: UsageModelMetric,
    /// Segment the range slider's thumb travels from on the next paint: the
    /// range that was selected before the latest switch. Registered when the
    /// range changes so a switch reads as one move instead of a jump.
    range_thumb_from: usize,
    table: Option<Entity<TableState<UsageTableDelegate>>>,
    generation: u64,
    refresh_task: Option<Task<()>>,
}

impl Default for UsageView {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageView {
    pub fn new() -> Self {
        let request = AgentUsageStatisticsRequest::default();
        Self {
            backend: None,
            range_thumb_from: usage_range_index(request.range),
            request,
            statistics: None,
            loading: false,
            stale: false,
            error: None,
            trend_view: UsageTrendView::Bars,
            enabled_trend_metrics: vec![
                AgentUsageTrendMetric::InputTokens,
                AgentUsageTrendMetric::OutputTokens,
                AgentUsageTrendMetric::CachedTokens,
            ],
            model_metric: UsageModelMetric::TotalTokens,
            table: None,
            generation: 0,
            refresh_task: None,
        }
    }

    pub fn set_backend(&mut self, backend: BackendFacade, cx: &mut Context<Self>) {
        self.backend = Some(backend);
        self.refresh(cx);
    }

    pub fn clear_backend(&mut self, cx: &mut Context<Self>) {
        self.backend = None;
        self.statistics = None;
        self.loading = false;
        self.stale = false;
        self.error = None;
        self.generation = self.generation.saturating_add(1);
        self.refresh_task = None;
        cx.notify();
    }

    pub fn activate(&mut self, session_filter: Option<VibexSessionId>, cx: &mut Context<Self>) {
        let next_sessions = session_filter.into_iter().collect::<Vec<_>>();
        let filter_changed = self.request.session_ids != next_sessions;
        if filter_changed {
            self.request.session_ids = next_sessions;
        }
        if filter_changed || self.statistics.is_none() || self.stale {
            self.refresh(cx);
        } else {
            cx.notify();
        }
    }

    pub fn invalidate(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.stale = true;
        if visible {
            self.refresh(cx);
        } else {
            cx.notify();
        }
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        self.loading = true;
        self.stale = self.statistics.is_some();
        self.error = None;
        let request = self.request.clone();
        let entity = cx.weak_entity();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend.agent().usage_statistics(request).await
        });
        self.refresh_task = Some(cx.spawn(async move |_, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match outcome {
                    Ok(Ok(statistics)) => {
                        this.statistics = Some(statistics);
                        this.stale = false;
                        this.error = None;
                    }
                    Ok(Err(error)) => {
                        this.error = Some((error.code, error.message));
                        this.stale = this.statistics.is_some();
                    }
                    Err(error) => {
                        this.error = Some((
                            "agent_usage_refresh_task_failed".to_string(),
                            error.to_string(),
                        ));
                        this.stale = this.statistics.is_some();
                    }
                }
                this.sync_table_delegate(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn choose_range(&mut self, range: AgentUsageRange, cx: &mut Context<Self>) {
        if self.request.range != range {
            // The slider's thumb starts its travel where the old selection sat.
            self.range_thumb_from = usage_range_index(self.request.range);
            self.request.range = range;
            self.refresh(cx);
        }
    }

    fn choose_dimension(&mut self, dimension: AgentUsageDimension, cx: &mut Context<Self>) {
        if self.request.dimension != dimension {
            self.request.dimension = dimension;
            // The leading table column is named after the dimension, so the
            // stored column groups must re-derive before the next paint.
            self.sync_table_delegate(cx);
            self.refresh(cx);
        }
    }

    fn choose_trend_view(&mut self, view: UsageTrendView, cx: &mut Context<Self>) {
        if self.trend_view != view {
            self.trend_view = view;
            cx.notify();
        }
    }

    fn toggle_trend_metric(&mut self, metric: AgentUsageTrendMetric, cx: &mut Context<Self>) {
        if metric == AgentUsageTrendMetric::TotalTokens {
            if self.enabled_trend_metrics.contains(&metric) {
                self.enabled_trend_metrics.clear();
            } else {
                self.enabled_trend_metrics.clear();
                self.enabled_trend_metrics.push(metric);
            }
        } else {
            self.enabled_trend_metrics
                .retain(|current| *current != AgentUsageTrendMetric::TotalTokens);
            toggle_typed(&mut self.enabled_trend_metrics, metric);
        }
        cx.notify();
    }

    fn choose_model_metric(&mut self, metric: UsageModelMetric, cx: &mut Context<Self>) {
        if self.model_metric != metric {
            self.model_metric = metric;
            cx.notify();
        }
    }

    fn apply_table_sort(
        &mut self,
        metric: AgentUsageSortMetric,
        direction: AgentUsageSortDirection,
        cx: &mut Context<Self>,
    ) {
        if self.request.sort_metric != metric || self.request.sort_direction != direction {
            self.request.sort_metric = metric;
            self.request.sort_direction = direction;
            self.refresh(cx);
        }
    }

    fn clear_filter(&mut self, kind: UsageFilterKind, cx: &mut Context<Self>) {
        match kind {
            UsageFilterKind::Agent => self.request.agent_ids.clear(),
            UsageFilterKind::Project => self.request.project_ids.clear(),
            UsageFilterKind::ProviderProfile => self.request.provider_profile_ids.clear(),
            UsageFilterKind::Model => self.request.model_ids.clear(),
            UsageFilterKind::Session => self.request.session_ids.clear(),
        }
        self.refresh(cx);
    }

    fn toggle_filter(&mut self, kind: UsageFilterKind, id: String, cx: &mut Context<Self>) {
        match kind {
            UsageFilterKind::Agent => {
                if let Ok(id) = AgentId::parse(id) {
                    toggle_typed(&mut self.request.agent_ids, id);
                }
            }
            UsageFilterKind::Project => {
                if let Ok(id) = ProjectId::parse(id) {
                    toggle_typed(&mut self.request.project_ids, id);
                }
            }
            UsageFilterKind::ProviderProfile => {
                if let Ok(id) = ProviderProfileId::parse(id) {
                    toggle_typed(&mut self.request.provider_profile_ids, id);
                }
            }
            UsageFilterKind::Model => toggle_typed(&mut self.request.model_ids, id),
            UsageFilterKind::Session => {
                if let Ok(id) = VibexSessionId::parse(id) {
                    toggle_typed(&mut self.request.session_ids, id);
                }
            }
        }
        self.refresh(cx);
    }

    fn render_toolbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let range_control = self.render_range_slider(window, cx);

        let options = self
            .statistics
            .as_ref()
            .map(|statistics| statistics.filter_options.clone())
            .unwrap_or_default();
        // Scope first, cross-filters second: the range control owns the leading
        // edge and the filters trail the row as one group, so the toolbar reads
        // as two decisions instead of six peer buttons. Both groups wrap, and a
        // wrapped group starts at the leading edge rather than overlapping.
        h_flex()
            .w_full()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .child(range_control)
            .child(
                h_flex()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        self.render_filter_button(
                            UsageFilterKind::Agent,
                            locale::text("Agent", "Agent", "Agent"),
                            options.agents,
                            self.request
                                .agent_ids
                                .iter()
                                .map(|id| id.as_str().to_string())
                                .collect(),
                            cx,
                        ),
                    )
                    .child(
                        self.render_filter_button(
                            UsageFilterKind::ProviderProfile,
                            locale::text("Model provider", "模型供应商", "模型供應商"),
                            options.provider_profiles,
                            self.request
                                .provider_profile_ids
                                .iter()
                                .map(|id| id.as_str().to_string())
                                .collect(),
                            cx,
                        ),
                    )
                    .child(self.render_filter_button(
                        UsageFilterKind::Model,
                        locale::text("Model", "模型", "模型"),
                        options.models,
                        self.request.model_ids.clone(),
                        cx,
                    ))
                    .child(
                        self.render_filter_button(
                            UsageFilterKind::Project,
                            locale::text("Project", "项目", "專案"),
                            options.projects,
                            self.request
                                .project_ids
                                .iter()
                                .map(|id| id.as_str().to_string())
                                .collect(),
                            cx,
                        ),
                    )
                    .child(
                        self.render_filter_button(
                            UsageFilterKind::Session,
                            locale::text("Session", "会话", "工作階段"),
                            options.sessions,
                            self.request
                                .session_ids
                                .iter()
                                .map(|id| id.as_str().to_string())
                                .collect(),
                            cx,
                        ),
                    ),
            )
            .into_any_element()
    }

    /// The time-range slider: one shell, one thumb that travels between the
    /// four range segments.
    ///
    /// The ranges are a single choice, so only the selected one carries a fill.
    /// The segments stay real [`Button`]s — focus, keyboard activation, and the
    /// announced selected and pressed states remain the component's — and take
    /// the width their label needs, so a locale that spells "30 days" wider
    /// gets a wider segment instead of a clipped one. The thumb is paint behind
    /// them: it reads where the segments landed and travels to the newly
    /// selected one over the app's movement spec.
    fn render_range_slider(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.request.range;
        let selected_index = usage_range_index(selected);
        let thumb_from = self.range_thumb_from;
        // Segment boxes are measured during prepaint, so the thumb can only be
        // placed on the frame after the first layout. Bumping this state is
        // what asks the window for that frame; until it arrives the selected
        // segment keeps the component's own fill so the choice stays visible.
        let slots = window
            .use_keyed_state("usage-range-slots", cx, |_, _| {
                Rc::new(RefCell::new(UsageRangeSlots::default()))
            })
            .read(cx)
            .clone();
        let measured = window.use_keyed_state("usage-range-measured", cx, |_, _| false);
        let thumb_boxes = slots.borrow().thumb(thumb_from, selected_index);
        if thumb_boxes.is_none() && !*measured.read(cx) {
            measured.update(cx, |value, _| *value = true);
        }
        // The pill steps its radius down from the shell's, so the two curves
        // stay concentric at the inset the shell pads by.
        let thumb_radius = (cx.theme().radius - px(USAGE_RANGE_INSET)).max(px(0.0));
        let foreground = cx.theme().foreground;
        let muted_foreground = cx.theme().muted_foreground;
        // Every switch gets its own animation element: a new id starts the
        // travel from the segment the range came from rather than resuming a
        // finished slide, and the element the switch replaced drops its state.
        let thumb_id =
            SharedString::from(format!("usage-range-thumb-{thumb_from}-{selected_index}"));
        let thumb = thumb_boxes.map(|(from, to)| {
            let travel = move |progress: f32| {
                (
                    motion::lerp(f32::from(from.origin.x), f32::from(to.origin.x), progress),
                    motion::lerp(
                        f32::from(from.size.width),
                        f32::from(to.size.width),
                        progress,
                    ),
                )
            };
            div()
                .debug_selector(|| "usage-range-thumb".to_string())
                .absolute()
                .top_0()
                .bottom_0()
                .with_animation(
                    thumb_id,
                    motion::SEGMENT_SLIDE.animation(),
                    move |thumb, progress| {
                        let (left, width) = travel(progress);
                        thumb.left(px(left)).w(px(width))
                    },
                )
                .child(
                    div()
                        .size_full()
                        .rounded(thumb_radius)
                        .bg(cx.theme().secondary),
                )
                .into_any_element()
        });
        let thumb_ready = thumb.is_some();
        let segments = h_flex().children(USAGE_RANGES.iter().enumerate().map(|(index, range)| {
            let is_selected = *range == selected;
            let segment_slots = slots.clone();
            // A measuring wrapper, because it is the segment's own box the
            // thumb has to land on.
            div()
                .flex_shrink_0()
                .debug_selector(move || format!("usage-range-segment-{index}"))
                .on_prepaint(move |bounds, _, _| {
                    segment_slots.borrow_mut().set_segment(index, bounds);
                })
                .child(
                    Button::new(SharedString::from(format!("usage-range-{range:?}")))
                        .accessibility_label(usage_range_label(*range))
                        // The label is a plain nowrap child rather than the
                        // button's own label slot: it must never ellipsize,
                        // because the segment is sized from it.
                        .child(
                            div()
                                .debug_selector(move || format!("usage-range-label-{index}"))
                                .whitespace_nowrap()
                                .child(usage_range_label(*range)),
                        )
                        .selected(is_selected)
                        .toggled(is_selected)
                        .on_click(cx.listener(move |this, _, _, cx| this.choose_range(*range, cx)))
                        .h(px(USAGE_RANGE_SEGMENT_HEIGHT))
                        .rounded(thumb_radius)
                        // The thumb owns the selected fill once it is placed; a
                        // button background would double it.
                        .when(thumb_ready || !is_selected, |button| {
                            button.bg(transparent_black())
                        })
                        .text_color(if is_selected {
                            foreground
                        } else {
                            muted_foreground
                        }),
                )
        }));
        div()
            .debug_selector(|| "usage-range-slider".to_string())
            .rounded(cx.theme().radius)
            .border(px(USAGE_RANGE_BORDER))
            .border_color(cx.theme().border)
            .p(px(USAGE_RANGE_INSET))
            .child(
                h_flex()
                    .relative()
                    .on_prepaint({
                        let slots = slots.clone();
                        move |bounds, _, _| slots.borrow_mut().set_track(bounds)
                    })
                    .children(thumb)
                    .child(segments),
            )
            .into_any_element()
    }

    fn render_filter_button(
        &self,
        kind: UsageFilterKind,
        label: &'static str,
        options: Vec<AgentUsageFilterOption>,
        selected: Vec<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected_count = selected.len();
        // The trigger names the applied value, so an active filter is readable
        // without opening its menu; the dimension name moves into the
        // accessibility label once a value replaces it on the button.
        let trigger_label =
            usage_filter_trigger_label(label, options.as_slice(), selected.as_slice());
        let accessibility_label = if selected_count == 0 {
            trigger_label.clone()
        } else {
            format!("{label}: {trigger_label}")
        };
        let all_label = match locale::current_locale() {
            locale::ResolvedLocale::En => format!("All {label}"),
            locale::ResolvedLocale::ZhCn => format!("全部{label}"),
            locale::ResolvedLocale::ZhTw => format!("全部{label}"),
        };
        let entity = cx.weak_entity();
        Button::new(SharedString::from(format!("usage-filter-{kind:?}")))
            .small()
            .outline()
            .selected(selected_count > 0)
            .debug_selector(move || format!("usage-filter-{kind:?}"))
            // The page runs its controls a step quieter than the component's
            // outline default: the shared hairline keeps the toolbar's buttons
            // in the same family as the range slider's shell.
            .border_color(cx.theme().border)
            // The trigger is a labeled menu, so it takes the component's own
            // icon slot and caret instead of ad-hoc children: both scale with
            // the control size and follow its hover, pressed, and selected
            // states.
            .icon(usage_filter_icon(kind).opacity(0.72))
            .dropdown_caret(true)
            // The visible content is the applied value, so the announced name
            // states the dimension it filters.
            .accessibility_label(accessibility_label)
            .child(
                div()
                    .min_w_0()
                    .max_w(px(150.0))
                    .truncate()
                    .child(trigger_label),
            )
            .disabled(options.is_empty() && selected.is_empty())
            .dropdown_menu(move |menu, _, _| {
                let clear_entity = entity.clone();
                let mut menu = menu
                    .when(kind == UsageFilterKind::Session, |menu| {
                        menu.min_w(px(USAGE_SESSION_FILTER_MENU_WIDTH))
                            .max_w(px(USAGE_SESSION_FILTER_MENU_WIDTH))
                    })
                    .item(
                        PopupMenuItem::new(all_label.clone())
                            .checked(selected.is_empty())
                            .on_click(move |_, _, cx| {
                                let _ =
                                    clear_entity.update(cx, |this, cx| this.clear_filter(kind, cx));
                            }),
                    );
                for option in options.iter().cloned() {
                    let checked = selected.contains(&option.id);
                    let id = option.id;
                    let option_entity = entity.clone();
                    let item_element_id =
                        SharedString::from(format!("usage-filter-{kind:?}-option-{id}"));
                    let label = option.label;
                    let item = if kind == UsageFilterKind::Session {
                        let display_label = bounded_usage_session_filter_label(&label);
                        let tooltip_label = label.clone();
                        PopupMenuItem::element(move |_, _| {
                            let tooltip_label = tooltip_label.clone();
                            div()
                                .id(item_element_id.clone())
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .aria_label(label.clone())
                                .child(display_label.clone())
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tooltip_label.clone()).build(window, cx)
                                })
                        })
                    } else {
                        PopupMenuItem::new(label)
                    };
                    let item = item.checked(checked).on_click(move |_, _, cx| {
                        let id = id.clone();
                        let _ =
                            option_entity.update(cx, |this, cx| this.toggle_filter(kind, id, cx));
                    });
                    menu = menu.item(item);
                }
                menu
            })
            .into_any_element()
    }

    fn render_summary(
        &self,
        aggregate: &AgentUsageAggregate,
        viewport_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let columns = summary_columns(viewport_width);
        div()
            .grid()
            .grid_cols(columns)
            .w_full()
            .gap_3()
            .children([
                summary_metric_value(
                    "total",
                    locale::text("Total tokens", "总 Token", "總 Token"),
                    IconName::Cpu,
                    &aggregate.total_tokens,
                    cx,
                ),
                summary_metric(
                    "requests",
                    // Turns and API requests differ by two orders of magnitude
                    // on agentic adapters, so the tile names whichever it shows.
                    match aggregate.api_requests {
                        Some(_) => locale::text("API requests", "API 请求数", "API 請求數"),
                        None => locale::text("Turns", "对话轮次", "對話輪次"),
                    },
                    IconName::Inbox,
                    format_compact_number(aggregate.api_requests.unwrap_or(aggregate.requests)),
                    false,
                    cx,
                ),
                summary_metric_value(
                    "input",
                    locale::text("Input", "输入", "輸入"),
                    IconName::ArrowDown,
                    &aggregate.input_tokens,
                    cx,
                ),
                summary_metric_value(
                    "output",
                    locale::text("Output", "输出", "輸出"),
                    IconName::ArrowUp,
                    &aggregate.output_tokens,
                    cx,
                ),
                summary_metric_value(
                    "cached",
                    locale::text("Cached read", "缓存读取", "快取讀取"),
                    IconName::HardDrive,
                    &aggregate.cached_tokens,
                    cx,
                ),
                summary_metric(
                    "cache-hit",
                    locale::text("Cache hit rate", "缓存命中率", "快取命中率"),
                    IconName::ChartPie,
                    format_basis_points(aggregate.cache_hit_rate.basis_points),
                    aggregate.cache_hit_rate.basis_points.is_none(),
                    cx,
                ),
            ])
            .into_any_element()
    }

    fn render_trend(
        &mut self,
        statistics: &AgentUsageStatistics,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let trend_view = self.trend_view;
        let chart = match trend_view {
            UsageTrendView::Bars => {
                render_stacked_trend(statistics, self.enabled_trend_metrics.as_slice(), cx)
            }
            UsageTrendView::Heatmap => render_usage_heatmap(statistics.annual.as_ref(), cx),
            UsageTrendView::Models => {
                render_model_usage(statistics.annual.as_ref(), self.model_metric, cx)
            }
        };
        let trailing_control = match trend_view {
            UsageTrendView::Bars => Some(self.render_trend_legend(cx)),
            UsageTrendView::Models => Some(self.render_model_metric_control(cx)),
            UsageTrendView::Heatmap => None,
        };
        usage_card(cx)
            .gap_3()
            .px_4()
            .py_3()
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_sm().font_semibold().child(locale::text(
                                "Usage trend",
                                "用量趋势",
                                "用量趨勢",
                            )))
                            .child(
                                ButtonGroup::new("usage-trend-view-toggle")
                                    .small()
                                    .outline()
                                    .child(
                                        usage_outline_option(
                                            "usage-trend-view-bars",
                                            trend_view == UsageTrendView::Bars,
                                            cx,
                                        )
                                        .icon(IconName::ChartPie)
                                        .label(locale::text("Trend", "趋势", "趨勢")),
                                    )
                                    .child(
                                        usage_outline_option(
                                            "usage-trend-view-heatmap",
                                            trend_view == UsageTrendView::Heatmap,
                                            cx,
                                        )
                                        .icon(IconName::LayoutDashboard)
                                        .label(locale::text("Heatmap", "热力", "熱力")),
                                    )
                                    .child(
                                        usage_outline_option(
                                            "usage-trend-view-models",
                                            trend_view == UsageTrendView::Models,
                                            cx,
                                        )
                                        .icon(IconName::ChartPie)
                                        .label(locale::text("Models", "模型", "模型")),
                                    )
                                    .on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
                                        if selected.contains(&0) {
                                            this.choose_trend_view(UsageTrendView::Bars, cx);
                                        } else if selected.contains(&1) {
                                            this.choose_trend_view(UsageTrendView::Heatmap, cx);
                                        } else if selected.contains(&2) {
                                            this.choose_trend_view(UsageTrendView::Models, cx);
                                        }
                                    })),
                            ),
                    )
                    .children(trailing_control),
            )
            .child(chart)
            .into_any_element()
    }

    fn render_trend_legend(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut legend = h_flex().flex_wrap().items_center().justify_end().gap_1();
        for series in usage_trend_series(cx) {
            let active = self.enabled_trend_metrics.contains(&series.metric);
            let metric = series.metric;
            let label = series.label;
            let button = Button::new(SharedString::from(format!(
                "usage-trend-series-{:?}",
                series.metric
            )))
            .xsmall()
            .ghost()
            .compact()
            .selected(active)
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(div().size(px(7.0)).rounded_full().bg(series.color))
                    .child(label),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_trend_metric(metric, cx)));
            legend = legend.child(button_with_aria_label(button, label));
        }
        legend.into_any_element()
    }

    fn render_model_metric_control(&mut self, cx: &mut Context<Self>) -> AnyElement {
        ButtonGroup::new("usage-model-metric")
            .small()
            .outline()
            .child(
                usage_outline_option(
                    "usage-model-metric-requests",
                    self.model_metric == UsageModelMetric::Requests,
                    cx,
                )
                .label(locale::text("Turns", "对话轮次", "對話輪次")),
            )
            .child(
                usage_outline_option(
                    "usage-model-metric-tokens",
                    self.model_metric == UsageModelMetric::TotalTokens,
                    cx,
                )
                .label(locale::text("Total tokens", "总 Token", "總 Token")),
            )
            .on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
                if selected.contains(&0) {
                    this.choose_model_metric(UsageModelMetric::Requests, cx);
                } else if selected.contains(&1) {
                    this.choose_model_metric(UsageModelMetric::TotalTokens, cx);
                }
            }))
            .into_any_element()
    }

    fn render_dimensions(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let dimensions = [
            (
                AgentUsageDimension::Time,
                locale::text("Time", "时间", "時間"),
            ),
            (
                AgentUsageDimension::Agent,
                locale::text("Agent", "Agent", "Agent"),
            ),
            (
                AgentUsageDimension::Project,
                locale::text("Project", "项目", "專案"),
            ),
            (
                AgentUsageDimension::ModelProvider,
                locale::text("Model provider", "模型供应商", "模型供應商"),
            ),
            (
                AgentUsageDimension::Model,
                locale::text("Model", "模型", "模型"),
            ),
        ];
        let selected = self.request.dimension;
        let mut controls = h_flex().w_full().flex_wrap().gap(px(2.0));
        for (dimension, label) in dimensions {
            controls = controls.child(
                Button::new(SharedString::from(format!("usage-dimension-{dimension:?}")))
                    .small()
                    .ghost()
                    // The tab strip keeps its own row height while the control
                    // takes the standard small padding.
                    .h(px(28.0))
                    .selected(dimension == selected)
                    .label(label)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.choose_dimension(dimension, cx)),
                    ),
            );
        }
        usage_card(cx)
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_center()
                    .gap(px(2.0))
                    .px_2()
                    .py(px(6.0))
                    .border_b_1()
                    .border_color(cx.theme().border.opacity(0.55))
                    .child(controls),
            )
            .child(self.render_usage_table(window, cx))
            .into_any_element()
    }

    /// Push the state the table delegate paints into the `TableState`.
    ///
    /// The delegate owns its snapshot so that neither the table's refresh nor
    /// its render has to read back into this view, which would panic while
    /// the view is leased.
    fn sync_table_delegate(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.table.clone() else {
            return;
        };
        let dimension = self.request.dimension;
        let sort_metric = self.request.sort_metric;
        let sort_direction = self.request.sort_direction;
        let rows = self
            .statistics
            .as_ref()
            .map(|statistics| statistics.dimension_rows.clone())
            .unwrap_or_default();
        table.update(cx, |table, cx| {
            if table
                .delegate_mut()
                .apply_snapshot(dimension, sort_metric, sort_direction, rows)
            {
                table.refresh(cx);
                cx.notify();
            }
        });
    }

    fn render_usage_table(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let row_count = self
            .statistics
            .as_ref()
            .map(|statistics| statistics.dimension_rows.len())
            .unwrap_or(0);
        let table = match &self.table {
            Some(table) => table.clone(),
            None => {
                let delegate = UsageTableDelegate {
                    view: cx.weak_entity(),
                    dimension: self.request.dimension,
                    sort_metric: self.request.sort_metric,
                    sort_direction: self.request.sort_direction,
                    rows: self
                        .statistics
                        .as_ref()
                        .map(|statistics| statistics.dimension_rows.clone())
                        .unwrap_or_default(),
                };
                let table = cx.new(|cx| {
                    TableState::new(delegate, window, cx)
                        .col_movable(false)
                        .col_resizable(false)
                        .row_selectable(false)
                        .col_selectable(false)
                });
                self.table = Some(table.clone());
                table
            }
        };
        div()
            .w_full()
            .debug_selector(|| "usage-table".to_string())
            .h(px(usage_table_height(row_count)))
            .child(
                DataTable::new(&table)
                    .stripe(true)
                    .bordered(false)
                    .scrollbar_visible(false, true)
                    .with_size(Size::Size(px(USAGE_TABLE_ROW_HEIGHT))),
            )
            .into_any_element()
    }

    fn render_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some((code, message)) = self.error.as_ref() {
            let unsupported = code == "agent_usage_statistics_unavailable";
            let title = if unsupported {
                locale::text(
                    "Usage statistics unavailable",
                    "用量统计不可用",
                    "用量統計不可用",
                )
            } else if self.statistics.is_some() {
                locale::text(
                    "Refresh failed; showing previous data",
                    "刷新失败，正在显示上次数据",
                    "重新整理失敗，正在顯示上次資料",
                )
            } else {
                locale::text(
                    "Usage statistics could not be loaded",
                    "无法加载用量统计",
                    "無法載入用量統計",
                )
            };
            return Some(
                h_flex()
                    .w_full()
                    .min_h(px(38.0))
                    .items_center()
                    .gap_2()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(cx.theme().warning.opacity(0.45))
                    .bg(cx.theme().warning.opacity(0.08))
                    .px_3()
                    .text_xs()
                    .child(Icon::new(IconName::TriangleAlert).size(px(14.0)))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(format!("{title}: {message}")),
                    )
                    .into_any_element(),
            );
        }
        None
    }
}

/// Card chrome shared by the usage panels and their loading placeholders: the
/// card plate, a hairline border, and the large radius. Padding and clipping
/// stay with the caller — the trend card is inset, the dimension card clips its
/// table.
fn usage_card(cx: &App) -> gpui::Div {
    v_flex()
        .w_full()
        .min_w_0()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .bg(theme::semantic_color("card", cx.theme().is_dark()).opacity(0.72))
}

impl Render for UsageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport_width = f32::from(window.viewport_size().width);
        let status = self.render_status(cx);
        let statistics = self.statistics.clone();
        let state = usage_content_state(
            statistics
                .as_ref()
                .map(|statistics| statistics.totals.requests),
            self.loading,
            self.error.is_some(),
        );
        let content =
            match state {
                UsageContentState::Ready => {
                    let statistics = statistics.expect("ready usage state requires statistics");
                    v_flex()
                        .w_full()
                        .gap_4()
                        .children(status)
                        .child(self.render_toolbar(window, cx))
                        .child(self.render_summary(&statistics.totals, viewport_width, cx))
                        .child(self.render_trend(&statistics, cx))
                        .child(self.render_dimensions(window, cx))
                        .into_any_element()
                }
                UsageContentState::Loading => {
                    // Stand in for the ready layout with its own containers — the
                    // same toolbar slot, summary grid, and two cards — so the
                    // statistics land where their placeholders stood instead of
                    // reflowing the page. This is a first load only: once
                    // `statistics` is set the state is `Ready`, so a range, filter,
                    // or sort change keeps the previous numbers on screen.
                    v_flex()
                        .w_full()
                        .gap_4()
                        .children(status)
                        .child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(skeleton::skeleton_bar(
                                    USAGE_TOOLBAR_CONTROL_HEIGHT,
                                    0.22,
                                    cx,
                                ))
                                .child(skeleton::skeleton_bar(
                                    USAGE_TOOLBAR_CONTROL_HEIGHT,
                                    0.46,
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .grid()
                                .grid_cols(summary_columns(viewport_width))
                                .w_full()
                                .gap_3()
                                .children((0..USAGE_SUMMARY_METRIC_COUNT).map(|_| {
                                    skeleton::skeleton_bar(USAGE_SUMMARY_TILE_HEIGHT, 1.0, cx)
                                })),
                        )
                        .child(
                            usage_card(cx)
                                .gap_3()
                                .px_4()
                                .py_3()
                                .child(skeleton::skeleton_bar(24.0, 0.34, cx))
                                .child(skeleton::skeleton_bar(USAGE_CHART_HEIGHT, 1.0, cx)),
                        )
                        .child(
                            usage_card(cx)
                                .overflow_hidden()
                                .child(
                                    h_flex()
                                        .w_full()
                                        .items_center()
                                        .gap(px(2.0))
                                        .px_2()
                                        .py(px(6.0))
                                        .border_b_1()
                                        .border_color(cx.theme().border.opacity(0.55))
                                        .child(skeleton::skeleton_bar(28.0, 0.38, cx)),
                                )
                                .child(v_flex().w_full().children(
                                    (0..USAGE_LOADING_TABLE_ROWS).map(|_| {
                                        h_flex()
                                            .w_full()
                                            .h(px(USAGE_TABLE_ROW_HEIGHT))
                                            .items_center()
                                            .px_3()
                                            .child(skeleton::skeleton_bar(12.0, 0.42, cx))
                                    }),
                                )),
                        )
                        .into_any_element()
                }
                UsageContentState::Empty => v_flex()
                    .w_full()
                    .gap_4()
                    .children(status)
                    .child(self.render_toolbar(window, cx))
                    .child(
                        v_flex()
                            .h(px(200.0))
                            .w_full()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(Icon::new(IconName::Inbox).size(px(22.0)))
                            .child(locale::text(
                                "Usage is recorded from the time this feature is enabled",
                                "用量从启用此功能后开始记录",
                                "用量從啟用此功能後開始記錄",
                            )),
                    )
                    .into_any_element(),
                UsageContentState::Unavailable => v_flex()
                    .w_full()
                    .gap_4()
                    .children(status)
                    .child(self.render_toolbar(window, cx))
                    .child(centered_message(
                        locale::text(
                            "Usage data is not available",
                            "用量数据当前不可用",
                            "用量資料目前不可用",
                        ),
                        cx,
                    ))
                    .into_any_element(),
            };
        v_flex()
            .id("usage-view")
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .p_4()
                    .pb_6()
                    .child(content),
            )
    }
}

fn toggle_typed<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if let Some(index) = values.iter().position(|current| current == &value) {
        values.remove(index);
    } else {
        values.push(value);
    }
}

fn bounded_usage_session_filter_label(value: &str) -> String {
    let width_units = |character: char| if character.is_ascii() { 1 } else { 2 };
    let total_width = value.chars().map(width_units).sum::<usize>();
    if total_width <= USAGE_SESSION_FILTER_LABEL_MAX_WIDTH_UNITS {
        return value.to_string();
    }

    let content_width = USAGE_SESSION_FILTER_LABEL_MAX_WIDTH_UNITS.saturating_sub(3);
    let mut current_width = 0_usize;
    let mut output = String::new();
    for character in value.chars() {
        let character_width = width_units(character);
        if current_width.saturating_add(character_width) > content_width {
            break;
        }
        output.push(character);
        current_width = current_width.saturating_add(character_width);
    }
    output.push_str("...");
    output
}

/// Where the range slider's segments landed, in the slider track's coordinates.
///
/// The segments take the width their labels need, so the thumb cannot be placed
/// by a fraction of the track: it reads the boxes the segments reported during
/// prepaint, on the render that follows.
#[derive(Default)]
struct UsageRangeSlots {
    track: Option<Bounds<Pixels>>,
    segments: [Option<Bounds<Pixels>>; USAGE_RANGES.len()],
}

impl UsageRangeSlots {
    fn set_track(&mut self, bounds: Bounds<Pixels>) {
        self.track = Some(bounds);
    }

    fn set_segment(&mut self, index: usize, bounds: Bounds<Pixels>) {
        if let Some(slot) = self.segments.get_mut(index) {
            *slot = Some(bounds);
        }
    }

    /// The thumb's travel for a switch from `from` to `to`, insetted inside the
    /// segment box so the pill keeps the shell's padding. `None` until the
    /// slider has been laid out once.
    fn thumb(&self, from: usize, to: usize) -> Option<(Bounds<Pixels>, Bounds<Pixels>)> {
        Some((self.segment_box(from)?, self.segment_box(to)?))
    }

    fn segment_box(&self, index: usize) -> Option<Bounds<Pixels>> {
        let track = self.track?;
        let segment = self.segments.get(index).copied().flatten()?;
        let inset = px(USAGE_RANGE_INSET);
        let width = segment.size.width - px(2.0 * USAGE_RANGE_INSET);
        if width <= px(0.0) || segment.size.height <= px(0.0) || track.size.width <= px(0.0) {
            return None;
        }
        Some(Bounds {
            origin: point(segment.origin.x - track.origin.x + inset, px(0.0)),
            size: size(width, segment.size.height),
        })
    }
}

/// One option of a page-local outline [`ButtonGroup`].
///
/// The component draws an outline button's border with `input`, a step
/// brighter than the hairline the usage page runs its controls at; every
/// option takes the shared `border` token instead so the page holds one
/// border weight.
fn usage_outline_option(id: impl Into<ElementId>, selected: bool, cx: &App) -> Button {
    Button::new(id)
        .selected(selected)
        .border_color(cx.theme().border)
}

/// Position of `range` in the slider's segment order.
///
/// Unknown values fall back to the leading segment: the slider always has a
/// thumb, and a request the toolbar cannot place is still one of the four.
fn usage_range_index(range: AgentUsageRange) -> usize {
    USAGE_RANGES
        .iter()
        .position(|candidate| *candidate == range)
        .unwrap_or(0)
}

fn usage_range_label(range: AgentUsageRange) -> &'static str {
    match range {
        AgentUsageRange::Today => locale::text("Today", "今天", "今天"),
        AgentUsageRange::Last7Days => locale::text("7 days", "7 天", "7 天"),
        AgentUsageRange::Last30Days => locale::text("30 days", "30 天", "30 天"),
        AgentUsageRange::AllTime => locale::text("All", "全部", "全部"),
    }
}

/// Leading glyph for a cross-filter trigger.
///
/// The trigger names its active value rather than the dimension, so the icon
/// carries which dimension the button filters.
fn usage_filter_icon(kind: UsageFilterKind) -> Icon {
    match kind {
        UsageFilterKind::Agent => Icon::new(IconName::Bot),
        UsageFilterKind::Project => Icon::new(IconName::Folder),
        UsageFilterKind::ProviderProfile => Icon::default().path("icons/vibex/database.svg"),
        UsageFilterKind::Model => Icon::default().path("icons/vibex/sparkles.svg"),
        UsageFilterKind::Session => Icon::default().path("icons/vibex/message-square.svg"),
    }
}

/// Label a cross-filter trigger shows for its current selection.
///
/// An unfiltered trigger keeps the dimension name; a filtered one names the
/// first applied value and keeps the remaining count as a `+N` suffix, so the
/// trigger stays honest about how many values are applied. A selection whose
/// value the option list cannot name yet falls back to the dimension plus the
/// count.
fn usage_filter_trigger_label(
    dimension_label: &str,
    options: &[AgentUsageFilterOption],
    selected: &[String],
) -> String {
    let mut labels = selected.iter().filter_map(|id| {
        options
            .iter()
            .find(|option| &option.id == id)
            .map(|option| option.label.as_str())
    });
    let Some(first) = labels.next() else {
        return if selected.is_empty() {
            dimension_label.to_string()
        } else {
            format!("{dimension_label} ({})", selected.len())
        };
    };
    let remaining = labels.count();
    if remaining == 0 {
        first.to_string()
    } else {
        format!("{first} +{remaining}")
    }
}

fn summary_metric_value(
    id: &'static str,
    label: &'static str,
    icon: IconName,
    metric: &AgentUsageMetricValue,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let display_value = metric
        .value
        .map(|value| {
            // Partial coverage means the reported numbers cover only part of the
            // work behind these turns, so the figure is a floor, not the total.
            let formatted = format_compact_number(value);
            match metric.coverage {
                AgentUsageMetricCoverage::Partial => format!("≥ {formatted}"),
                _ => formatted,
            }
        })
        .unwrap_or_else(|| locale::text("Unknown", "未知", "未知").to_string());
    summary_metric(id, label, icon, display_value, metric.value.is_none(), cx)
}

fn summary_metric(
    id: &'static str,
    label: &'static str,
    icon: IconName,
    value: String,
    unknown: bool,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    v_flex()
        .id(SharedString::from(format!("usage-summary-{id}")))
        .min_w_0()
        .justify_center()
        .gap_2()
        .rounded(px(8.0))
        .border_1()
        .border_color(cx.theme().border)
        .bg(theme::semantic_color("card", cx.theme().is_dark()).opacity(0.72))
        .px_3p5()
        .py_3()
        .child(
            h_flex()
                .min_w_0()
                .items_center()
                .gap_1p5()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    h_flex()
                        .size(px(20.0))
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(px(5.0))
                        .bg(cx.theme().primary.opacity(0.08))
                        .child(
                            Icon::new(icon)
                                .size(px(12.0))
                                .text_color(cx.theme().primary.opacity(0.82)),
                        ),
                )
                .child(div().min_w_0().truncate().child(label)),
        )
        .child(
            div()
                .truncate()
                .text_xl()
                .line_height(gpui::relative(1.2))
                .font_semibold()
                .when(unknown, |this| this.text_color(cx.theme().muted_foreground))
                .child(value),
        )
        .into_any_element()
}

fn metric_coverage_label(coverage: AgentUsageMetricCoverage) -> &'static str {
    match coverage {
        AgentUsageMetricCoverage::Complete => locale::text("Reported", "已上报", "已回報"),
        AgentUsageMetricCoverage::Derived => locale::text(
            "Derived from input + output",
            "由输入 + 输出推导",
            "由輸入 + 輸出推導",
        ),
        AgentUsageMetricCoverage::Partial => locale::text("Partial", "部分上报", "部分回報"),
        AgentUsageMetricCoverage::Unknown => locale::text("Not reported", "未上报", "未回報"),
    }
}

fn trend_value(aggregate: &AgentUsageAggregate, metric: AgentUsageTrendMetric) -> Option<u64> {
    match metric {
        // Same rule as the summary tile: API requests when the adapters report
        // them, turns otherwise.
        AgentUsageTrendMetric::Requests => {
            Some(aggregate.api_requests.unwrap_or(aggregate.requests))
        }
        AgentUsageTrendMetric::TotalTokens => {
            token_trend_value(aggregate.requests, aggregate.total_tokens.value)
        }
        AgentUsageTrendMetric::InputTokens => {
            token_trend_value(aggregate.requests, aggregate.input_tokens.value)
        }
        AgentUsageTrendMetric::OutputTokens => {
            token_trend_value(aggregate.requests, aggregate.output_tokens.value)
        }
        AgentUsageTrendMetric::CachedTokens => {
            token_trend_value(aggregate.requests, aggregate.cached_tokens.value)
        }
    }
}

fn token_trend_value(requests: u64, value: Option<u64>) -> Option<u64> {
    if requests == 0 { Some(0) } else { value }
}

fn usage_trend_series(cx: &Context<UsageView>) -> [UsageTrendSeries; 4] {
    let is_dark = cx.theme().is_dark();
    [
        UsageTrendSeries {
            metric: AgentUsageTrendMetric::TotalTokens,
            label: locale::text("Total", "总量", "總量"),
            color: theme::semantic_color("chart-2", is_dark),
        },
        UsageTrendSeries {
            metric: AgentUsageTrendMetric::InputTokens,
            label: locale::text("Input", "输入", "輸入"),
            color: theme::semantic_color("chart-3", is_dark),
        },
        UsageTrendSeries {
            metric: AgentUsageTrendMetric::OutputTokens,
            label: locale::text("Output", "输出", "輸出"),
            color: theme::semantic_color("chart-4", is_dark),
        },
        UsageTrendSeries {
            metric: AgentUsageTrendMetric::CachedTokens,
            label: locale::text("Cache", "缓存", "快取"),
            color: theme::semantic_color("chart-5", is_dark),
        },
    ]
}

fn render_stacked_trend(
    statistics: &AgentUsageStatistics,
    enabled_metrics: &[AgentUsageTrendMetric],
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let series = usage_trend_series(cx)
        .into_iter()
        .filter(|series| enabled_metrics.contains(&series.metric))
        .collect::<Vec<_>>();
    let has_metrics = !series.is_empty();
    let buckets = statistics
        .trend_buckets
        .iter()
        .map(|bucket| {
            TrendChartBucket::new(
                bucket.label.clone(),
                series
                    .iter()
                    .map(|series| trend_value(&bucket.aggregate, series.metric))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let has_values = buckets.iter().any(|bucket| bucket.has_reported_value());
    let chart = TrendChart::new(
        buckets,
        series
            .into_iter()
            .map(|series| TrendChartSeries::new(series.label, series.color))
            .collect(),
    );

    // The chart owns its own axis, grid, and tooltip; the card only stacks the
    // empty-state message over it.
    div()
        .relative()
        .w_full()
        .min_w_0()
        .h(px(USAGE_CHART_HEIGHT))
        .child(chart)
        .when(!has_values, |this| {
            this.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if has_metrics {
                        locale::text(
                            "No reported values in this range",
                            "此范围内没有已上报数值",
                            "此範圍內沒有已回報數值",
                        )
                    } else {
                        locale::text(
                            "Select a metric to display",
                            "请选择要展示的维度",
                            "請選擇要顯示的維度",
                        )
                    }),
            )
        })
        .into_any_element()
}

fn usage_heatmap_entries(annual: &AgentUsageAnnualProjection) -> Vec<UsageHeatmapEntry> {
    annual
        .days
        .iter()
        .map(|day| UsageHeatmapEntry {
            label: day.label.clone(),
            value: token_trend_value(day.requests, day.total_tokens.value),
        })
        .collect()
}

fn heatmap_start_row(entries: &[UsageHeatmapEntry]) -> usize {
    entries
        .first()
        .and_then(|entry| NaiveDate::parse_from_str(&entry.label, "%Y-%m-%d").ok())
        .map(|date| date.weekday().num_days_from_monday() as usize)
        .unwrap_or(0)
}

fn heatmap_level(value: Option<u64>, maximum: u64) -> Option<u8> {
    value.map(|value| {
        if value == 0 || maximum == 0 {
            0
        } else {
            ((value as f64 / maximum as f64 * 4.0).ceil() as u8).clamp(1, 4)
        }
    })
}

fn heatmap_color(level: Option<u8>, cx: &Context<UsageView>) -> Hsla {
    let is_dark = cx.theme().is_dark();
    match level {
        None => theme::semantic_color("card", is_dark),
        Some(0) => cx.theme().muted.opacity(0.28),
        Some(1) => theme::semantic_color("chart-5", is_dark),
        Some(2) => theme::semantic_color("chart-4", is_dark),
        Some(3) => theme::semantic_color("chart-3", is_dark),
        Some(_) => theme::semantic_color("chart-1", is_dark),
    }
}

fn heatmap_weekday_label(row: usize) -> &'static str {
    match row {
        0 => locale::text("Mon", "一", "一"),
        2 => locale::text("Wed", "三", "三"),
        4 => locale::text("Fri", "五", "五"),
        _ => "",
    }
}

fn month_label(month: u32) -> String {
    const ENGLISH: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    match locale::current_locale() {
        locale::ResolvedLocale::En => ENGLISH
            .get(month.saturating_sub(1) as usize)
            .copied()
            .unwrap_or_default()
            .to_string(),
        locale::ResolvedLocale::ZhCn | locale::ResolvedLocale::ZhTw => format!("{month}月"),
    }
}

fn render_heatmap_legend(cx: &mut Context<UsageView>) -> AnyElement {
    let mut levels = h_flex().items_center().gap(px(3.0));
    for level in 0..=4 {
        levels = levels.child(
            div()
                .size(px(11.0))
                .rounded(px(2.0))
                .bg(heatmap_color(Some(level), cx)),
        );
    }
    h_flex()
        .w_full()
        .justify_end()
        .items_center()
        .gap_2()
        .text_size(px(10.0))
        .text_color(cx.theme().muted_foreground)
        .child(locale::text("Less", "少", "少"))
        .child(levels)
        .child(locale::text("More", "多", "多"))
        .into_any_element()
}

fn render_usage_heatmap(
    annual: Option<&AgentUsageAnnualProjection>,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let Some(annual) = annual else {
        return centered_message(
            locale::text(
                "Daily heatmap is unavailable",
                "每日热力图暂不可用",
                "每日熱力圖暫不可用",
            ),
            cx,
        );
    };
    let entries = usage_heatmap_entries(annual);
    if entries.is_empty() {
        return centered_message(
            locale::text(
                "No reported values in this range",
                "此范围内没有已上报数值",
                "此範圍內沒有已回報數值",
            ),
            cx,
        );
    }
    let maximum = entries
        .iter()
        .filter_map(|entry| entry.value)
        .max()
        .unwrap_or(0);
    let row_count = 7;
    let start_row = heatmap_start_row(&entries);
    let columns = (start_row + entries.len()).div_ceil(row_count).max(1);
    let mut weekday_labels = v_flex().flex_none().gap(px(USAGE_HEATMAP_GAP));
    for row in 0..7 {
        weekday_labels = weekday_labels.child(
            div()
                .h(px(USAGE_HEATMAP_CELL_SIZE))
                .w(px(28.0))
                .flex()
                .items_center()
                .text_size(px(10.0))
                .text_color(cx.theme().muted_foreground)
                .child(heatmap_weekday_label(row)),
        );
    }
    let mut month_labels = h_flex().items_start().gap(px(USAGE_HEATMAP_GAP));
    let mut previous_month = None;
    for column in 0..columns {
        let entry_index = column
            .checked_mul(row_count)
            .and_then(|slot| slot.checked_sub(start_row));
        let month = entry_index
            .and_then(|index| entries.get(index))
            .and_then(|entry| NaiveDate::parse_from_str(&entry.label, "%Y-%m-%d").ok())
            .map(|date| date.month());
        let label = if month.is_some() && month != previous_month {
            previous_month = month;
            month.map(month_label).unwrap_or_default()
        } else {
            String::new()
        };
        month_labels = month_labels.child(
            div()
                .w(px(USAGE_HEATMAP_CELL_SIZE))
                .h(px(14.0))
                .flex_none()
                .text_size(px(10.0))
                .text_color(cx.theme().muted_foreground)
                .child(label),
        );
    }
    let mut matrix = h_flex().items_start().gap(px(USAGE_HEATMAP_GAP));
    for column in 0..columns {
        let mut week = v_flex().gap(px(USAGE_HEATMAP_GAP));
        for row in 0..row_count {
            let slot = column * row_count + row;
            let entry_index = slot.checked_sub(start_row);
            if let Some(entry) = entry_index.and_then(|index| entries.get(index)) {
                let level = heatmap_level(entry.value, maximum);
                let tooltip = format!(
                    "{} · {} Token",
                    entry.label,
                    entry
                        .value
                        .map(format_full_number)
                        .unwrap_or_else(|| locale::text("Unknown", "未知", "未知").to_string())
                );
                week = week.child(
                    div()
                        .id(SharedString::from(format!("usage-heatmap-cell-{slot}")))
                        .size(px(USAGE_HEATMAP_CELL_SIZE))
                        .rounded(px(2.0))
                        .bg(heatmap_color(level, cx))
                        .when(level.is_none(), |this| {
                            this.border_1().border_color(cx.theme().border)
                        })
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)),
                );
            } else {
                week = week.child(div().size(px(USAGE_HEATMAP_CELL_SIZE)));
            }
        }
        matrix = matrix.child(week);
    }
    let calendar = h_flex()
        .w_full()
        .min_w_0()
        .items_start()
        .gap_2()
        .child(weekday_labels)
        .child(matrix);
    v_flex()
        .w_full()
        .min_h(px(USAGE_CHART_HEIGHT))
        .justify_center()
        .gap_3()
        .child(
            div()
                .id("usage-heatmap-scroll")
                .w_full()
                .overflow_x_scroll()
                .child(
                    v_flex()
                        .min_w(px(USAGE_HEATMAP_MIN_WIDTH))
                        .gap(px(2.0))
                        .child(
                            h_flex()
                                .items_start()
                                .gap_2()
                                .child(div().w(px(28.0)).flex_none())
                                .child(month_labels),
                        )
                        .child(calendar),
                ),
        )
        .child(render_heatmap_legend(cx))
        .into_any_element()
}

#[derive(Debug, Clone)]
struct UsageModelCategory {
    id: String,
    label: String,
    color: Hsla,
    other: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsageModelIdentity {
    id: String,
    label: String,
    other: bool,
}

fn model_metric_value(model: &AgentUsageDailyModelUsage, metric: UsageModelMetric) -> Option<u64> {
    match metric {
        UsageModelMetric::Requests => Some(model.requests),
        UsageModelMetric::TotalTokens => model.total_tokens.value,
    }
}

fn model_metric_label(metric: UsageModelMetric) -> &'static str {
    match metric {
        UsageModelMetric::Requests => locale::text("Turns", "对话轮次", "對話輪次"),
        UsageModelMetric::TotalTokens => locale::text("Total tokens", "总 Token", "總 Token"),
    }
}

fn usage_model_series_id(model: &AgentUsageDailyModelUsage) -> String {
    model
        .model_id
        .clone()
        .unwrap_or_else(|| USAGE_AGENT_DEFAULT_MODEL_ID.to_string())
}

fn model_category_color(index: usize, cx: &Context<UsageView>) -> Hsla {
    const TOKENS: [&str; USAGE_MODEL_LIMIT] = [
        "chart-category-1",
        "chart-category-2",
        "chart-category-3",
        "chart-category-4",
        "chart-category-5",
        "chart-category-6",
        "chart-category-7",
        "chart-category-8",
        "chart-category-9",
        "chart-category-10",
    ];
    theme::semantic_color(TOKENS[index.min(TOKENS.len() - 1)], cx.theme().is_dark())
}

fn ranked_model_identities(annual: &AgentUsageAnnualProjection) -> Vec<UsageModelIdentity> {
    let mut totals = BTreeMap::<String, (String, u64, Option<u64>)>::new();
    for day in &annual.days {
        for model in &day.models {
            let entry = totals
                .entry(usage_model_series_id(model))
                .or_insert_with(|| (model.label.clone(), 0, None));
            entry.1 = entry.1.saturating_add(model.requests);
            if let Some(value) = model.total_tokens.value {
                entry.2 = Some(entry.2.unwrap_or(0).saturating_add(value));
            }
        }
    }
    let mut ranked = totals.into_iter().collect::<Vec<_>>();
    ranked.sort_by(
        |(left_id, (left_label, left_requests, left_tokens)),
         (right_id, (right_label, right_requests, right_tokens))| {
            right_tokens
                .is_some()
                .cmp(&left_tokens.is_some())
                .then_with(|| right_tokens.cmp(left_tokens))
                .then_with(|| right_requests.cmp(left_requests))
                .then_with(|| left_label.to_lowercase().cmp(&right_label.to_lowercase()))
                .then_with(|| left_id.cmp(right_id))
        },
    );
    let needs_other = ranked.len() > USAGE_MODEL_LIMIT;
    let visible_count = if needs_other {
        USAGE_MODEL_LIMIT - 1
    } else {
        ranked.len()
    };
    let mut categories = ranked
        .into_iter()
        .take(visible_count)
        .map(|(id, (label, _, _))| UsageModelIdentity {
            id,
            label,
            other: false,
        })
        .collect::<Vec<_>>();
    if needs_other {
        categories.push(UsageModelIdentity {
            id: USAGE_OTHER_MODEL_ID.to_string(),
            label: locale::text("Other", "其他", "其他").to_string(),
            other: true,
        });
    }
    categories
}

fn model_categories(
    annual: &AgentUsageAnnualProjection,
    cx: &Context<UsageView>,
) -> Vec<UsageModelCategory> {
    ranked_model_identities(annual)
        .into_iter()
        .enumerate()
        .map(|(index, identity)| UsageModelCategory {
            id: identity.id,
            label: identity.label,
            color: model_category_color(index, cx),
            other: identity.other,
        })
        .collect()
}

fn model_day_value(
    day: &vibex_core::AgentUsageAnnualDay,
    category: &UsageModelCategory,
    metric: UsageModelMetric,
    visible_model_ids: &BTreeSet<String>,
) -> Option<u64> {
    let mut found = false;
    let mut known = false;
    let mut sum = 0_u64;
    for model in &day.models {
        let model_id = usage_model_series_id(model);
        let included = if category.other {
            !visible_model_ids.contains(&model_id)
        } else {
            model_id == category.id
        };
        if !included {
            continue;
        }
        found = true;
        if let Some(value) = model_metric_value(model, metric) {
            known = true;
            sum = sum.saturating_add(value);
        }
    }
    if !found {
        Some(0)
    } else if known {
        Some(sum)
    } else {
        None
    }
}

fn render_model_usage(
    annual: Option<&AgentUsageAnnualProjection>,
    metric: UsageModelMetric,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let Some(annual) = annual else {
        return centered_message(
            locale::text(
                "Model usage is unavailable",
                "模型用量暂不可用",
                "模型用量暫不可用",
            ),
            cx,
        );
    };
    let categories = model_categories(annual, cx);
    let visible_model_ids = categories
        .iter()
        .filter(|category| !category.other)
        .map(|category| category.id.clone())
        .collect::<BTreeSet<_>>();
    let mut previous_month = None;
    let days = annual
        .days
        .iter()
        .map(|day| {
            let month = NaiveDate::parse_from_str(&day.label, "%Y-%m-%d")
                .ok()
                .map(|date| date.month());
            // Only the first day of each month carries a label, so the band
            // axis reads as a calendar instead of a date smear.
            let month_label = (month.is_some() && month != previous_month)
                .then(|| SharedString::from(month.map(month_label).unwrap_or_default()));
            if month.is_some() {
                previous_month = month;
            }
            ModelChartDay::new(
                day.label.clone(),
                month_label,
                categories
                    .iter()
                    .map(|category| model_day_value(day, category, metric, &visible_model_ids))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let chart = ModelChart::new(
        days,
        categories
            .iter()
            .map(|category| ModelChartCategory::new(category.label.clone(), category.color))
            .collect(),
        model_metric_label(metric),
    );
    let mut legend = h_flex().w_full().flex_wrap().items_center().gap_2();
    for category in &categories {
        legend = legend.child(
            h_flex()
                .items_center()
                .gap_1()
                .child(div().size(px(8.0)).rounded(px(2.0)).bg(category.color))
                .child(div().text_xs().child(category.label.clone())),
        );
    }
    // The chart owns its percentage axis, month labels, and tooltip; the card
    // only keeps the horizontal scroll and the category legend around it.
    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .id("usage-model-chart-scroll")
                .w_full()
                .overflow_x_scroll()
                .child(
                    div()
                        .relative()
                        .min_w(px(USAGE_MODEL_CHART_MIN_WIDTH))
                        .h(px(USAGE_CHART_HEIGHT))
                        .child(chart),
                ),
        )
        .child(legend)
        .into_any_element()
}

fn dimension_label(dimension: AgentUsageDimension) -> &'static str {
    match dimension {
        AgentUsageDimension::Time => locale::text("Time", "时间", "時間"),
        AgentUsageDimension::Agent => locale::text("Agent", "Agent", "Agent"),
        AgentUsageDimension::Project => locale::text("Project", "项目", "專案"),
        AgentUsageDimension::ModelProvider => {
            locale::text("Model provider", "模型供应商", "模型供應商")
        }
        AgentUsageDimension::Model => locale::text("Model", "模型", "模型"),
    }
}

const USAGE_TABLE_LABEL_WIDTH: f32 = 280.0;
const USAGE_TABLE_ROW_HEIGHT: f32 = 42.0;
const USAGE_TABLE_HEADER_HEIGHT: f32 = 42.0;
const USAGE_TABLE_EMPTY_BODY_HEIGHT: f32 = 96.0;

/// Height that lets the `DataTable` paint every dimension row.
///
/// `DataTable` lays its body out through a virtualized `uniform_list` that
/// fills its parent, so without a definite container height the body collapses
/// to zero and only the header survives. The usage table is not a nested
/// vertical scroll surface: the page owns vertical scrolling, so the table is
/// sized to the rows it renders instead of a fixed viewport.
fn usage_table_height(row_count: usize) -> f32 {
    let body_height = if row_count == 0 {
        USAGE_TABLE_EMPTY_BODY_HEIGHT
    } else {
        row_count as f32 * USAGE_TABLE_ROW_HEIGHT
    };
    USAGE_TABLE_HEADER_HEIGHT + body_height
}

/// Renders the usage breakdown table.
///
/// The delegate owns every value it paints. It must not read back into
/// `UsageView`: the `TableState` renders and refreshes from inside that
/// view's own update, where reading the leased entity panics with
/// "cannot read `UsageView` while it is already being updated".
struct UsageTableDelegate {
    view: WeakEntity<UsageView>,
    dimension: AgentUsageDimension,
    sort_metric: AgentUsageSortMetric,
    sort_direction: AgentUsageSortDirection,
    rows: Vec<AgentUsageDimensionRow>,
}

impl UsageTableDelegate {
    fn apply_snapshot(
        &mut self,
        dimension: AgentUsageDimension,
        sort_metric: AgentUsageSortMetric,
        sort_direction: AgentUsageSortDirection,
        rows: Vec<AgentUsageDimensionRow>,
    ) -> bool {
        let columns_changed = self.dimension != dimension
            || self.sort_metric != sort_metric
            || self.sort_direction != sort_direction;
        let rows_changed = self.rows != rows;
        if columns_changed {
            self.dimension = dimension;
            self.sort_metric = sort_metric;
            self.sort_direction = sort_direction;
        }
        if rows_changed {
            self.rows = rows;
        }
        columns_changed || rows_changed
    }

    fn sort_target(col_ix: usize) -> Option<AgentUsageSortMetric> {
        Some(match col_ix {
            1 => AgentUsageSortMetric::Requests,
            2 => AgentUsageSortMetric::TotalTokens,
            3 => AgentUsageSortMetric::InputTokens,
            4 => AgentUsageSortMetric::OutputTokens,
            5 => AgentUsageSortMetric::CachedTokens,
            6 => AgentUsageSortMetric::CacheHitRate,
            7 => AgentUsageSortMetric::LastActivity,
            _ => return None,
        })
    }

    fn label_column(dimension: AgentUsageDimension) -> Column {
        Column::new("dimension", dimension_label(dimension))
            .width(px(USAGE_TABLE_LABEL_WIDTH))
            .paddings(Edges {
                top: px(0.),
                right: px(8.),
                bottom: px(0.),
                left: px(12.),
            })
    }

    fn value_column(key: &'static str, label: &'static str, width: f32) -> Column {
        Column::new(key, label)
            .width(px(width))
            .text_right()
            .paddings(Edges::all(px(8.)))
    }

    fn sortable_column(
        key: &'static str,
        label: &'static str,
        width: f32,
        metric: AgentUsageSortMetric,
        active_metric: AgentUsageSortMetric,
        direction: AgentUsageSortDirection,
    ) -> Column {
        let column = Self::value_column(key, label, width).sortable();
        if active_metric == metric {
            match direction {
                AgentUsageSortDirection::Ascending => column.ascending(),
                AgentUsageSortDirection::Descending => column.descending(),
            }
        } else {
            column
        }
    }
}

impl TableDelegate for UsageTableDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        9
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let sort_metric = self.sort_metric;
        let sort_direction = self.sort_direction;
        match col_ix {
            0 => Self::label_column(self.dimension),
            1 => Self::sortable_column(
                "requests",
                locale::text("Requests", "请求", "請求"),
                84.0,
                AgentUsageSortMetric::Requests,
                sort_metric,
                sort_direction,
            ),
            2 => Self::sortable_column(
                "total",
                locale::text("Total", "总量", "總量"),
                108.0,
                AgentUsageSortMetric::TotalTokens,
                sort_metric,
                sort_direction,
            ),
            3 => Self::sortable_column(
                "input",
                locale::text("Input", "输入", "輸入"),
                100.0,
                AgentUsageSortMetric::InputTokens,
                sort_metric,
                sort_direction,
            ),
            4 => Self::sortable_column(
                "output",
                locale::text("Output", "输出", "輸出"),
                100.0,
                AgentUsageSortMetric::OutputTokens,
                sort_metric,
                sort_direction,
            ),
            5 => Self::sortable_column(
                "cached",
                locale::text("Cache", "缓存", "快取"),
                100.0,
                AgentUsageSortMetric::CachedTokens,
                sort_metric,
                sort_direction,
            ),
            6 => Self::sortable_column(
                "cache-hit-rate",
                locale::text("Hit rate", "命中率", "命中率"),
                92.0,
                AgentUsageSortMetric::CacheHitRate,
                sort_metric,
                sort_direction,
            ),
            7 => Self::sortable_column(
                "last-activity",
                locale::text("Last activity", "最近活动", "最近活動"),
                126.0,
                AgentUsageSortMetric::LastActivity,
                sort_metric,
                sort_direction,
            ),
            _ => Self::value_column(
                "coverage",
                locale::text("Coverage", "上报覆盖", "回報覆蓋"),
                118.0,
            ),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(metric) = Self::sort_target(col_ix) else {
            return;
        };
        // Cycling past ascending resets the table to its default ordering.
        let (metric, direction) = match sort {
            ColumnSort::Ascending => (metric, AgentUsageSortDirection::Ascending),
            ColumnSort::Descending => (metric, AgentUsageSortDirection::Descending),
            ColumnSort::Default => (
                AgentUsageSortMetric::default(),
                AgentUsageSortDirection::default(),
            ),
        };
        let _ = self
            .view
            .update(cx, |view, cx| view.apply_table_sort(metric, direction, cx));
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let name = self.column(col_ix, cx).name;
        if col_ix == 0 {
            return div().size_full().child(name).into_any_element();
        }
        div()
            .size_full()
            .flex()
            .justify_end()
            .child(name)
            .into_any_element()
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .h(px(96.0))
            .w_full()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(locale::text(
                "No usage facts match these filters",
                "没有符合筛选条件的用量记录",
                "沒有符合篩選條件的用量記錄",
            ))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(row) = self.rows.get(row_ix) else {
            return div().into_any_element();
        };
        let aggregate = &row.aggregate;
        let id = SharedString::from(format!("usage-cell-{row_ix}-{col_ix}"));
        match col_ix {
            0 => usage_table_label_cell(id, row).into_any_element(),
            1 => usage_table_value_cell(
                id,
                format_compact_number(aggregate.requests),
                false,
                Some(format!(
                    "{}: {}. {}",
                    locale::text("Requests", "请求", "請求"),
                    format_full_number(aggregate.requests),
                    locale::text(
                        "Dispatched prompt executions",
                        "实际发送的 prompt 执行数",
                        "實際傳送的 prompt 執行數",
                    )
                )),
                cx,
            )
            .into_any_element(),
            2 => usage_table_metric_cell(
                id,
                locale::text("Total tokens", "总 Token", "總 Token"),
                &aggregate.total_tokens,
                cx,
            )
            .into_any_element(),
            3 => usage_table_metric_cell(
                id,
                locale::text("Input tokens", "输入 Token", "輸入 Token"),
                &aggregate.input_tokens,
                cx,
            )
            .into_any_element(),
            4 => usage_table_metric_cell(
                id,
                locale::text("Output tokens", "输出 Token", "輸出 Token"),
                &aggregate.output_tokens,
                cx,
            )
            .into_any_element(),
            5 => usage_table_metric_cell(
                id,
                locale::text("Cached read tokens", "缓存读取 Token", "快取讀取 Token"),
                &aggregate.cached_tokens,
                cx,
            )
            .into_any_element(),
            6 => usage_table_value_cell(
                id,
                format_basis_points(aggregate.cache_hit_rate.basis_points),
                aggregate.cache_hit_rate.basis_points.is_none(),
                Some(format!(
                    "{}: {}. {}",
                    locale::text("Cache hit rate", "缓存命中率", "快取命中率"),
                    format_basis_points(aggregate.cache_hit_rate.basis_points),
                    cache_hit_detail(&aggregate.cache_hit_rate)
                )),
                cx,
            )
            .into_any_element(),
            7 => usage_table_value_cell(
                id,
                aggregate
                    .last_activity_at_ms
                    .map(format_timestamp)
                    .unwrap_or_else(|| "-".to_string()),
                aggregate.last_activity_at_ms.is_none(),
                aggregate.last_activity_at_ms.map(|timestamp| {
                    format!(
                        "{}: {}",
                        locale::text("Last activity", "最近活动", "最近活動"),
                        format_timestamp(timestamp)
                    )
                }),
                cx,
            )
            .into_any_element(),
            _ => usage_table_coverage_cell(id, aggregate, cx).into_any_element(),
        }
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _cx: &App) -> String {
        let Some(row) = self.rows.get(row_ix) else {
            return String::new();
        };
        let aggregate = &row.aggregate;
        match col_ix {
            0 => row.label.clone(),
            1 => format_compact_number(aggregate.requests),
            2 => usage_metric_compact_text(&aggregate.total_tokens),
            3 => usage_metric_compact_text(&aggregate.input_tokens),
            4 => usage_metric_compact_text(&aggregate.output_tokens),
            5 => usage_metric_compact_text(&aggregate.cached_tokens),
            6 => format_basis_points(aggregate.cache_hit_rate.basis_points),
            7 => aggregate
                .last_activity_at_ms
                .map(format_timestamp)
                .unwrap_or_else(|| "-".to_string()),
            _ => coverage_compact_label(&aggregate.coverage).to_string(),
        }
    }
}

fn usage_table_label_cell(id: SharedString, row: &AgentUsageDimensionRow) -> AnyElement {
    let tooltip = row.label.clone();
    div()
        .id(id)
        .h_full()
        .w_full()
        .flex()
        .items_center()
        .child(
            div()
                .truncate()
                .text_sm()
                .font_medium()
                .child(row.label.clone()),
        )
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .into_any_element()
}

fn usage_table_value_cell(
    id: SharedString,
    value: String,
    unknown: bool,
    tooltip: Option<String>,
    cx: &App,
) -> AnyElement {
    let element = div()
        .id(id)
        .h_full()
        .w_full()
        .flex()
        .items_center()
        .justify_end()
        .child(
            div()
                .truncate()
                .text_xs()
                .when(unknown, |this| this.text_color(cx.theme().muted_foreground))
                .child(value),
        );
    match tooltip {
        Some(tooltip) => element
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .into_any_element(),
        None => element.into_any_element(),
    }
}

fn usage_table_metric_cell(
    id: SharedString,
    label: &'static str,
    metric: &AgentUsageMetricValue,
    cx: &App,
) -> AnyElement {
    let tooltip = format!(
        "{label}: {}. {}",
        metric_full_value(metric),
        metric_detail(metric)
    );
    usage_table_value_cell(
        id,
        usage_metric_compact_text(metric),
        metric.value.is_none(),
        Some(tooltip),
        cx,
    )
}

fn usage_table_coverage_cell(
    id: SharedString,
    aggregate: &AgentUsageAggregate,
    cx: &App,
) -> AnyElement {
    let tooltip = coverage_detail(aggregate);
    div()
        .id(id)
        .h_full()
        .w_full()
        .flex()
        .items_center()
        .justify_end()
        .child(
            div()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(coverage_compact_label(&aggregate.coverage)),
        )
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .into_any_element()
}

fn usage_metric_compact_text(metric: &AgentUsageMetricValue) -> String {
    metric
        .value
        .map(format_compact_number)
        .unwrap_or_else(|| "-".to_string())
}

fn usage_content_state(requests: Option<u64>, loading: bool, has_error: bool) -> UsageContentState {
    match requests {
        Some(_) => UsageContentState::Ready,
        None if loading => UsageContentState::Loading,
        None if has_error => UsageContentState::Unavailable,
        None => UsageContentState::Empty,
    }
}

fn summary_columns(viewport_width: f32) -> u16 {
    if viewport_width >= 1180.0 {
        6
    } else if viewport_width >= 720.0 {
        3
    } else {
        2
    }
}

fn centered_message(message: &'static str, cx: &mut Context<UsageView>) -> AnyElement {
    div()
        .h(px(180.0))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(message)
        .into_any_element()
}

fn metric_full_value(metric: &AgentUsageMetricValue) -> String {
    metric
        .value
        .map(format_full_number)
        .unwrap_or_else(|| locale::text("Unknown", "未知", "未知").to_string())
}

fn metric_detail(metric: &AgentUsageMetricValue) -> String {
    if metric.derived_requests > 0 {
        return match locale::current_locale() {
            locale::ResolvedLocale::En => format!(
                "{}; known for {} of {} requests, including {} derived from input + output",
                metric_coverage_label(metric.coverage),
                metric.known_requests,
                metric.total_requests,
                metric.derived_requests
            ),
            locale::ResolvedLocale::ZhCn => format!(
                "{}；{} 个请求中有 {} 个已知，其中 {} 个由输入 + 输出推导",
                metric_coverage_label(metric.coverage),
                metric.total_requests,
                metric.known_requests,
                metric.derived_requests
            ),
            locale::ResolvedLocale::ZhTw => format!(
                "{}；{} 個請求中有 {} 個已知，其中 {} 個由輸入 + 輸出推導",
                metric_coverage_label(metric.coverage),
                metric.total_requests,
                metric.known_requests,
                metric.derived_requests
            ),
        };
    }
    match locale::current_locale() {
        locale::ResolvedLocale::En => format!(
            "{}; reported for {} of {} requests",
            metric_coverage_label(metric.coverage),
            metric.known_requests,
            metric.total_requests
        ),
        locale::ResolvedLocale::ZhCn => format!(
            "{}；{} 个请求中有 {} 个上报",
            metric_coverage_label(metric.coverage),
            metric.total_requests,
            metric.known_requests
        ),
        locale::ResolvedLocale::ZhTw => format!(
            "{}；{} 個請求中有 {} 個回報",
            metric_coverage_label(metric.coverage),
            metric.total_requests,
            metric.known_requests
        ),
    }
}

fn cache_hit_detail(rate: &vibex_core::AgentUsageCacheHitRate) -> String {
    match locale::current_locale() {
        locale::ResolvedLocale::En => format!(
            "{}; {} of {} requests eligible; {} cached read tokens / {} input + cached read tokens",
            metric_coverage_label(rate.coverage),
            rate.eligible_requests,
            rate.total_requests,
            format_full_number(rate.cached_read_tokens),
            format_full_number(rate.denominator_tokens)
        ),
        locale::ResolvedLocale::ZhCn => format!(
            "{}；{} 个请求中有 {} 个可计算；{} 缓存读取 Token / {} 输入与缓存读取 Token",
            metric_coverage_label(rate.coverage),
            rate.total_requests,
            rate.eligible_requests,
            format_full_number(rate.cached_read_tokens),
            format_full_number(rate.denominator_tokens)
        ),
        locale::ResolvedLocale::ZhTw => format!(
            "{}；{} 個請求中有 {} 個可計算；{} 快取讀取 Token / {} 輸入與快取讀取 Token",
            metric_coverage_label(rate.coverage),
            rate.total_requests,
            rate.eligible_requests,
            format_full_number(rate.cached_read_tokens),
            format_full_number(rate.denominator_tokens)
        ),
    }
}

fn coverage_compact_label(coverage: &vibex_core::AgentUsageCoverageSummary) -> String {
    if coverage.total_requests > 0 && coverage.complete_requests == coverage.total_requests {
        locale::text("Complete", "完整", "完整").to_string()
    } else {
        format!(
            "{}/{}",
            coverage
                .complete_requests
                .saturating_add(coverage.partial_requests),
            coverage.total_requests
        )
    }
}

fn coverage_detail(aggregate: &AgentUsageAggregate) -> String {
    let coverage = &aggregate.coverage;
    let reporting = match locale::current_locale() {
        locale::ResolvedLocale::En => format!(
            "Reporting: {} complete, {} partial, {} baseline only, {} unreported, {} unsupported",
            coverage.complete_requests,
            coverage.partial_requests,
            coverage.baseline_only_requests,
            coverage.unreported_requests,
            coverage.unsupported_requests
        ),
        locale::ResolvedLocale::ZhCn => format!(
            "上报覆盖：{} 完整，{} 部分，{} 仅基线，{} 未上报，{} 不支持",
            coverage.complete_requests,
            coverage.partial_requests,
            coverage.baseline_only_requests,
            coverage.unreported_requests,
            coverage.unsupported_requests
        ),
        locale::ResolvedLocale::ZhTw => format!(
            "回報覆蓋：{} 完整，{} 部分，{} 僅基線，{} 未回報，{} 不支援",
            coverage.complete_requests,
            coverage.partial_requests,
            coverage.baseline_only_requests,
            coverage.unreported_requests,
            coverage.unsupported_requests
        ),
    };
    format!(
        "{reporting}. {}: {}. {}: {}",
        locale::text("Thought tokens", "思考 Token", "思考 Token"),
        metric_full_value(&aggregate.thought_tokens),
        locale::text("Cached write tokens", "缓存写入 Token", "快取寫入 Token"),
        metric_full_value(&aggregate.cached_write_tokens)
    )
}

fn format_compact_number(value: u64) -> String {
    if value >= 1_000_000_000 {
        format!("{:.1}B", value as f64 / 1_000_000_000.0)
    } else if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

pub(crate) fn format_full_number(value: u64) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
}

fn format_basis_points(value: Option<u32>) -> String {
    match value {
        Some(value) if value % 100 == 0 => format!("{}%", value / 100),
        Some(value) => format!("{:.1}%", value as f64 / 100.0),
        None => "-".to_string(),
    }
}

fn format_timestamp(timestamp_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(timestamp_ms)
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage_metric(value: u64, requests: u64) -> AgentUsageMetricValue {
        AgentUsageMetricValue {
            value: Some(value),
            coverage: AgentUsageMetricCoverage::Complete,
            known_requests: requests,
            derived_requests: 0,
            total_requests: requests,
        }
    }

    fn usage_aggregate(requests: u64) -> AgentUsageAggregate {
        AgentUsageAggregate {
            requests,
            api_requests: Some(requests),
            total_tokens: usage_metric(requests * 100, requests),
            input_tokens: usage_metric(requests * 60, requests),
            output_tokens: usage_metric(requests * 40, requests),
            cached_tokens: usage_metric(requests * 10, requests),
            thought_tokens: usage_metric(0, requests),
            cached_write_tokens: usage_metric(0, requests),
            cache_hit_rate: vibex_core::AgentUsageCacheHitRate {
                basis_points: Some(2_500),
                cached_read_tokens: requests * 10,
                denominator_tokens: requests * 40,
                eligible_requests: requests,
                total_requests: requests,
                coverage: AgentUsageMetricCoverage::Complete,
            },
            coverage: vibex_core::AgentUsageCoverageSummary {
                complete_requests: requests,
                total_requests: requests,
                ..Default::default()
            },
            last_activity_at_ms: Some(1_700_000_000_000),
        }
    }

    fn usage_statistics() -> AgentUsageStatistics {
        AgentUsageStatistics {
            generated_at_ms: 1_700_000_000_000,
            effective_range: vibex_core::AgentUsageEffectiveRange {
                start_at_ms: 0,
                end_at_ms: 86_400_000,
                bucket_kind: "day".to_string(),
            },
            totals: usage_aggregate(3),
            trend_buckets: vec![vibex_core::AgentUsageTrendBucket {
                id: "0".to_string(),
                label: "2026-09-10".to_string(),
                start_at_ms: 0,
                end_at_ms: 86_400_000,
                aggregate: usage_aggregate(3),
            }],
            dimension_rows: vec![
                AgentUsageDimensionRow {
                    id: "agent-a".to_string(),
                    label: "Agent A".to_string(),
                    aggregate: usage_aggregate(2),
                },
                AgentUsageDimensionRow {
                    id: "agent-b".to_string(),
                    label: "Agent B".to_string(),
                    aggregate: usage_aggregate(1),
                },
            ],
            filter_options: vibex_core::AgentUsageFilterOptions::default(),
            annual: None,
        }
    }

    #[gpui::test]
    fn usage_table_renders_and_reloads_columns_without_reading_the_view(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| UsageView::new());
        view.update(cx, |view, cx| {
            view.statistics = Some(usage_statistics());
            view.loading = false;
            view.stale = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // The DataTable body only paints rows when its container has a definite
        // height, so the two fixture rows must reserve header plus two rows.
        let table_bounds = cx
            .debug_bounds("usage-table")
            .expect("usage table should be laid out");
        assert_eq!(table_bounds.size.height, px(usage_table_height(2)));
        assert!(
            table_bounds.size.height > px(USAGE_TABLE_HEADER_HEIGHT),
            "usage table body must be taller than its header"
        );

        // Switching dimension re-derives the table columns from inside the
        // view's own update, where the delegate must not read the view back.
        view.update(cx, |view, cx| {
            view.choose_dimension(AgentUsageDimension::Model, cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    #[test]
    fn usage_range_slider_covers_every_offered_range() {
        for (index, range) in USAGE_RANGES.iter().enumerate() {
            assert_eq!(usage_range_index(*range), index);
            assert!(!usage_range_label(*range).is_empty());
        }
        assert_eq!(USAGE_RANGES.len(), 4);

        // The toolbar keeps one shell with one moving thumb: the segments must
        // not fall back to per-segment outline fills.
        let source = include_str!("usage.rs");
        let slider = source
            .split_once("    fn render_range_slider(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_filter_button("))
            .map(|(body, _)| body)
            .expect("usage range slider should remain inspectable");
        assert!(slider.contains(".with_animation("));
        assert!(slider.contains("motion::SEGMENT_SLIDE"));
        assert!(slider.contains("set_segment("));
    }

    #[gpui::test]
    fn usage_range_slider_thumb_lands_on_the_selected_segment(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, _| UsageView::new());
        view.update(cx, |view, cx| {
            view.statistics = Some(usage_statistics());
            view.loading = false;
            view.stale = false;
            cx.notify();
        });
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }

        let slider = cx
            .debug_bounds("usage-range-slider")
            .expect("usage range slider should be laid out");
        // The slider sits level with the filter buttons beside it: one segment
        // plus the shell's inset and hairline on both edges.
        let filter = cx
            .debug_bounds("usage-filter-Agent")
            .expect("usage agent filter should be laid out");
        assert_eq!(slider.size.height, filter.size.height);
        assert_eq!(slider.size.height, px(24.0));
        let thumb = cx
            .debug_bounds("usage-range-thumb")
            .expect("usage range thumb should be placed once the segments are measured");
        assert_eq!(thumb.size.height, px(USAGE_RANGE_SEGMENT_HEIGHT));

        // The segments are laid out in range order, each one as wide as its
        // label needs, and the track is exactly their sum.
        let segments: Vec<Bounds<Pixels>> = [
            "usage-range-segment-0",
            "usage-range-segment-1",
            "usage-range-segment-2",
            "usage-range-segment-3",
        ]
        .into_iter()
        .map(|selector| {
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} should be laid out"))
        })
        .collect();
        let mut track_width = px(0.0);
        for pair in segments.windows(2) {
            assert_eq!(pair[1].origin.x, pair[0].origin.x + pair[0].size.width);
        }
        for segment in &segments {
            assert!(segment.size.width > px(0.0));
            track_width += segment.size.width;
        }
        let track_inset = px(USAGE_RANGE_INSET + USAGE_RANGE_BORDER);
        assert_eq!(segments[0].origin.x - slider.origin.x, track_inset);
        assert_eq!(slider.size.width, track_width + track_inset * 2.0);

        // The thumb is the selected segment's box, insetted by the shell's
        // padding, and it travels with the selection instead of the selected
        // button painting its own fill.
        // Every segment takes the width its label needs, with the same button
        // padding around it: a clipped label would collapse that difference.
        let labels: Vec<Bounds<Pixels>> = [
            "usage-range-label-0",
            "usage-range-label-1",
            "usage-range-label-2",
            "usage-range-label-3",
        ]
        .into_iter()
        .map(|selector| {
            cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} should be laid out"))
        })
        .collect();
        for (segment, label) in segments.iter().zip(&labels) {
            let padding = segment.size.width - label.size.width;
            assert_eq!(padding, segments[0].size.width - labels[0].size.width);
            assert!(padding > px(0.0));
        }

        let selected_index = view.update(cx, |view, _| usage_range_index(view.request.range));
        assert_eq!(
            thumb.origin.x,
            segments[selected_index].origin.x + px(USAGE_RANGE_INSET)
        );
        assert_eq!(
            thumb.size.width,
            segments[selected_index].size.width - px(2.0 * USAGE_RANGE_INSET)
        );

        cx.update(|_, cx| cx.set_reduce_motion(true));
        view.update(cx, |view, cx| {
            view.choose_range(AgentUsageRange::AllTime, cx);
        });
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }
        let thumb = cx
            .debug_bounds("usage-range-thumb")
            .expect("usage range thumb should stay placed");
        assert_eq!(thumb.origin.x, segments[3].origin.x + px(USAGE_RANGE_INSET));
        assert_eq!(
            thumb.size.width,
            segments[3].size.width - px(2.0 * USAGE_RANGE_INSET)
        );
    }

    #[test]
    fn compact_numbers_and_rates_are_stable() {
        assert_eq!(format_compact_number(999), "999");
        assert_eq!(format_compact_number(1_250), "1.2K");
        assert_eq!(format_compact_number(2_500_000), "2.5M");
        assert_eq!(format_basis_points(Some(7_500)), "75%");
        assert_eq!(format_basis_points(Some(3_333)), "33.3%");
        assert_eq!(format_basis_points(None), "-");
    }

    #[test]
    fn filters_toggle_without_zero_filling_other_dimensions() {
        let mut values = vec!["one".to_string()];
        toggle_typed(&mut values, "two".to_string());
        assert_eq!(values, ["one", "two"]);
        toggle_typed(&mut values, "one".to_string());
        assert_eq!(values, ["two"]);
    }

    #[test]
    fn filter_trigger_names_the_active_value_without_losing_the_count() {
        let options = vec![
            AgentUsageFilterOption {
                id: "agent-a".to_string(),
                label: "Codex".to_string(),
            },
            AgentUsageFilterOption {
                id: "agent-b".to_string(),
                label: "Claude Code".to_string(),
            },
        ];

        assert_eq!(
            usage_filter_trigger_label("Agent", &options, &[]),
            "Agent",
            "an unfiltered trigger keeps the dimension name"
        );
        assert_eq!(
            usage_filter_trigger_label("Agent", &options, &["agent-a".to_string()]),
            "Codex"
        );
        assert_eq!(
            usage_filter_trigger_label(
                "Agent",
                &options,
                &["agent-a".to_string(), "agent-b".to_string()]
            ),
            "Codex +1"
        );
        assert_eq!(
            usage_filter_trigger_label(
                "Agent",
                &options,
                &["agent-a".to_string(), "gone".to_string()]
            ),
            "Codex",
            "a selection outside the offered options must not leak into the label"
        );
        assert_eq!(
            usage_filter_trigger_label("Agent", &options, &["gone".to_string()]),
            "Agent (1)",
            "a selection the option list cannot name still reports that it filters"
        );
    }

    #[test]
    fn successful_query_keeps_fixed_year_views_visible_when_range_is_empty() {
        assert_eq!(
            usage_content_state(Some(0), false, false),
            UsageContentState::Ready
        );
        assert_eq!(
            usage_content_state(Some(2), true, false),
            UsageContentState::Ready
        );
        assert_eq!(
            usage_content_state(None, true, false),
            UsageContentState::Loading
        );
        assert_eq!(
            usage_content_state(None, false, true),
            UsageContentState::Unavailable
        );
    }

    #[test]
    fn retained_data_refresh_does_not_insert_a_transient_status_row() {
        let source = include_str!("usage.rs");
        let status = source
            .split_once("    fn render_status(")
            .and_then(|(_, tail)| tail.split_once("\n}\n\nimpl Render for UsageView"))
            .map(|(body, _)| body)
            .expect("usage status rendering should remain inspectable");

        assert!(!status.contains("self.stale"));
        assert!(!status.contains("Refreshing after new usage was committed"));
    }

    #[test]
    fn session_filter_menu_constrains_and_truncates_long_titles() {
        let source = include_str!("usage.rs");
        let filter_menu = source
            .split_once("    fn render_filter_button(")
            .and_then(|(_, tail)| tail.split_once("\n    fn render_summary("))
            .map(|(body, _)| body)
            .expect("usage filter menu rendering should remain inspectable");

        assert!(filter_menu.contains(".min_w(px(USAGE_SESSION_FILTER_MENU_WIDTH))"));
        assert!(filter_menu.contains(".max_w(px(USAGE_SESSION_FILTER_MENU_WIDTH))"));
        assert!(filter_menu.contains("if kind == UsageFilterKind::Session"));
        assert!(filter_menu.contains("bounded_usage_session_filter_label(&label)"));
        assert!(filter_menu.contains(".child(display_label.clone())"));
    }

    #[test]
    fn session_filter_title_bound_handles_ascii_and_cjk_text() {
        assert_eq!(
            bounded_usage_session_filter_label("Short title"),
            "Short title"
        );

        let ascii = bounded_usage_session_filter_label(&"a".repeat(80));
        assert_eq!(ascii, format!("{}...", "a".repeat(45)));

        let cjk = bounded_usage_session_filter_label(&"会话标题".repeat(12));
        assert!(cjk.ends_with("..."));
        assert_eq!(
            cjk.chars()
                .map(|character| if character.is_ascii() { 1 } else { 2 })
                .sum::<usize>(),
            47
        );
    }

    #[test]
    fn usage_layout_and_tooltip_helpers_preserve_narrow_and_exact_values() {
        assert_eq!(summary_columns(1_400.0), 6);
        assert_eq!(summary_columns(900.0), 3);
        assert_eq!(summary_columns(520.0), 2);

        let metric = AgentUsageMetricValue {
            value: Some(12_345_678),
            coverage: AgentUsageMetricCoverage::Partial,
            known_requests: 2,
            derived_requests: 1,
            total_requests: 3,
        };
        assert_eq!(metric_full_value(&metric), "12,345,678");
        let detail = metric_detail(&metric);
        assert!(detail.contains('2'));
        assert!(detail.contains('3'));
        assert!(detail.contains("input + output"));
    }

    #[test]
    fn heatmap_levels_preserve_unknown_zero_and_daily_alignment() {
        assert_eq!(token_trend_value(0, None), Some(0));
        assert_eq!(token_trend_value(1, None), None);
        assert_eq!(token_trend_value(1, Some(0)), Some(0));
        assert_eq!(heatmap_level(None, 100), None);
        assert_eq!(heatmap_level(Some(0), 100), Some(0));
        assert_eq!(heatmap_level(Some(25), 100), Some(1));
        assert_eq!(heatmap_level(Some(26), 100), Some(2));
        assert_eq!(heatmap_level(Some(75), 100), Some(3));
        assert_eq!(heatmap_level(Some(100), 100), Some(4));

        let entries = vec![UsageHeatmapEntry {
            label: "2024-01-03".to_string(),
            value: Some(1),
        }];
        assert_eq!(heatmap_start_row(&entries), 2);
    }

    #[test]
    fn model_view_keeps_nine_ranked_models_plus_other_in_a_stable_order() {
        let metric = |value| AgentUsageMetricValue {
            value: Some(value),
            coverage: AgentUsageMetricCoverage::Complete,
            known_requests: 1,
            derived_requests: 0,
            total_requests: 1,
        };
        let models = (0..11)
            .map(|index| AgentUsageDailyModelUsage {
                model_id: Some(format!("model-{index:02}")),
                label: format!("Model {index:02}"),
                requests: index + 1,
                total_tokens: metric((11 - index) * 100),
            })
            .collect();
        let annual = AgentUsageAnnualProjection {
            effective_range: vibex_core::AgentUsageEffectiveRange {
                start_at_ms: 0,
                end_at_ms: 1,
                bucket_kind: "day".to_string(),
            },
            days: vec![vibex_core::AgentUsageAnnualDay {
                id: "0".to_string(),
                label: "2026-07-31".to_string(),
                start_at_ms: 0,
                end_at_ms: 1,
                requests: 66,
                total_tokens: metric(6_600),
                models,
            }],
        };

        let categories = ranked_model_identities(&annual);
        assert_eq!(categories.len(), USAGE_MODEL_LIMIT);
        assert_eq!(categories[0].id, "model-00");
        assert!(categories.last().unwrap().other);
    }
}

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike as _, NaiveDate};
use gpui::{
    AnyElement, BorderStyle, Bounds, Context, Edges, Hsla, InteractiveElement as _, IntoElement,
    Render, ScrollHandle, ScrollWheelEvent, SharedString, Styled as _, Task, Window, canvas, div,
    point, prelude::*, px, quad, transparent_black,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _,
    button::{Button, ButtonGroup, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
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

use crate::{gpui_ext::button_with_aria_label, locale, theme};

const USAGE_HEADER_HEIGHT: f32 = 48.0;
const USAGE_CHART_HEIGHT: f32 = 176.0;
const USAGE_CHART_AXIS_WIDTH: f32 = 48.0;
const USAGE_HEATMAP_CELL_SIZE: f32 = 12.0;
const USAGE_HEATMAP_GAP: f32 = 3.0;
const USAGE_HEATMAP_MIN_WIDTH: f32 = 840.0;
const USAGE_MODEL_CHART_MIN_WIDTH: f32 = 720.0;
const USAGE_TABLE_MIN_WIDTH: f32 = 1040.0;
const USAGE_MODEL_LIMIT: usize = 10;
const USAGE_OTHER_MODEL_ID: &str = "__vibex_other_models__";
const USAGE_AGENT_DEFAULT_MODEL_ID: &str = "__vibex_agent_default_model__";

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
    table_scroll: ScrollHandle,
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
        Self {
            backend: None,
            request: AgentUsageStatisticsRequest::default(),
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
            table_scroll: ScrollHandle::new(),
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

    fn refresh(&mut self, cx: &mut Context<Self>) {
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
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn choose_range(&mut self, range: AgentUsageRange, cx: &mut Context<Self>) {
        if self.request.range != range {
            self.request.range = range;
            self.refresh(cx);
        }
    }

    fn choose_dimension(&mut self, dimension: AgentUsageDimension, cx: &mut Context<Self>) {
        if self.request.dimension != dimension {
            self.request.dimension = dimension;
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

    fn choose_sort(&mut self, metric: AgentUsageSortMetric, cx: &mut Context<Self>) {
        if self.request.sort_metric == metric {
            self.request.sort_direction = match self.request.sort_direction {
                AgentUsageSortDirection::Ascending => AgentUsageSortDirection::Descending,
                AgentUsageSortDirection::Descending => AgentUsageSortDirection::Ascending,
            };
        } else {
            self.request.sort_metric = metric;
            self.request.sort_direction = AgentUsageSortDirection::Descending;
        }
        self.refresh(cx);
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

    fn render_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let loading = self.loading;
        h_flex()
            .h(px(USAGE_HEADER_HEIGHT))
            .w_full()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .px_4()
            .child(
                h_flex()
                    .min_w_0()
                    .gap_2()
                    .child(
                        Icon::default()
                            .path("icons/vibex/activity.svg")
                            .size(px(17.0)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_semibold()
                            .child(locale::text("Usage Statistics", "用量统计", "用量統計")),
                    ),
            )
            .child(
                Button::new("usage-refresh")
                    .small()
                    .ghost()
                    .compact()
                    .size(px(30.0))
                    .loading(loading)
                    .tooltip(locale::text("Refresh", "刷新", "重新整理"))
                    .child(
                        Icon::default()
                            .path("icons/vibex/rotate-ccw.svg")
                            .size(px(15.0)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .into_any_element()
    }

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let ranges = [
            (
                AgentUsageRange::Today,
                locale::text("Today", "今天", "今天"),
            ),
            (
                AgentUsageRange::Last7Days,
                locale::text("7 days", "7 天", "7 天"),
            ),
            (
                AgentUsageRange::Last30Days,
                locale::text("30 days", "30 天", "30 天"),
            ),
            (
                AgentUsageRange::AllTime,
                locale::text("All", "全部", "全部"),
            ),
        ];
        let selected_range = self.request.range;
        let mut range_control = h_flex()
            .flex_none()
            .gap_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(cx.theme().border)
            .p(px(2.0));
        for (range, label) in ranges {
            range_control = range_control.child(
                Button::new(SharedString::from(format!("usage-range-{range:?}")))
                    .xsmall()
                    .ghost()
                    .h(px(28.0))
                    .selected(range == selected_range)
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_range(range, cx))),
            );
        }

        let options = self
            .statistics
            .as_ref()
            .map(|statistics| statistics.filter_options.clone())
            .unwrap_or_default();
        h_flex()
            .w_full()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(range_control)
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
        let button_label = if selected_count == 0 {
            label.to_string()
        } else {
            format!("{label} ({selected_count})")
        };
        let all_label = match locale::current_locale() {
            locale::ResolvedLocale::En => format!("All {label}"),
            locale::ResolvedLocale::ZhCn => format!("全部{label}"),
            locale::ResolvedLocale::ZhTw => format!("全部{label}"),
        };
        let entity = cx.weak_entity();
        Button::new(SharedString::from(format!("usage-filter-{kind:?}")))
            .xsmall()
            .ghost()
            .h(px(32.0))
            .px_2()
            .selected(selected_count > 0)
            .child(
                h_flex()
                    .min_w_0()
                    .gap_1()
                    .child(div().max_w(px(150.0)).truncate().child(button_label))
                    .child(Icon::new(IconName::ChevronDown).size(px(13.0))),
            )
            .disabled(options.is_empty() && selected.is_empty())
            .dropdown_menu(move |menu, _, _| {
                let clear_entity = entity.clone();
                let mut menu = menu.item(
                    PopupMenuItem::new(all_label.clone())
                        .checked(selected.is_empty())
                        .on_click(move |_, _, cx| {
                            let _ = clear_entity.update(cx, |this, cx| this.clear_filter(kind, cx));
                        }),
                );
                for option in options.iter().cloned() {
                    let checked = selected.contains(&option.id);
                    let id = option.id;
                    let option_entity = entity.clone();
                    menu = menu.item(PopupMenuItem::new(option.label).checked(checked).on_click(
                        move |_, _, cx| {
                            let id = id.clone();
                            let _ = option_entity
                                .update(cx, |this, cx| this.toggle_filter(kind, id, cx));
                        },
                    ));
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
            .gap_2()
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
        v_flex()
            .w_full()
            .gap_3()
            .border_t_1()
            .border_b_1()
            .border_color(cx.theme().border)
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
                            .gap_1()
                            .child(div().text_sm().font_semibold().child(locale::text(
                                "Usage trend",
                                "用量趋势",
                                "用量趨勢",
                            )))
                            .child(
                                ButtonGroup::new("usage-trend-view-toggle")
                                    .xsmall()
                                    .outline()
                                    .compact()
                                    .child(
                                        Button::new("usage-trend-view-bars")
                                            .icon(IconName::ChartPie)
                                            .label(locale::text("Trend", "趋势", "趨勢"))
                                            .selected(trend_view == UsageTrendView::Bars),
                                    )
                                    .child(
                                        Button::new("usage-trend-view-heatmap")
                                            .icon(IconName::LayoutDashboard)
                                            .label(locale::text("Heatmap", "热力", "熱力"))
                                            .selected(trend_view == UsageTrendView::Heatmap),
                                    )
                                    .child(
                                        Button::new("usage-trend-view-models")
                                            .icon(IconName::ChartPie)
                                            .label(locale::text("Models", "模型", "模型"))
                                            .selected(trend_view == UsageTrendView::Models),
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
            .xsmall()
            .outline()
            .compact()
            .child(
                Button::new("usage-model-metric-requests")
                    .label(locale::text("Turns", "对话轮次", "對話輪次"))
                    .selected(self.model_metric == UsageModelMetric::Requests),
            )
            .child(
                Button::new("usage-model-metric-tokens")
                    .label(locale::text("Total tokens", "总 Token", "總 Token"))
                    .selected(self.model_metric == UsageModelMetric::TotalTokens),
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

    fn render_dimensions(
        &mut self,
        statistics: &AgentUsageStatistics,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
        let mut controls = h_flex().w_full().flex_wrap().gap_1();
        for (dimension, label) in dimensions {
            controls = controls.child(
                Button::new(SharedString::from(format!("usage-dimension-{dimension:?}")))
                    .xsmall()
                    .ghost()
                    .h(px(30.0))
                    .selected(dimension == selected)
                    .label(label)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.choose_dimension(dimension, cx)),
                    ),
            );
        }
        v_flex()
            .w_full()
            .gap_2()
            .child(controls)
            .child(self.render_table(statistics.dimension_rows.as_slice(), cx))
            .into_any_element()
    }

    fn render_table(
        &mut self,
        rows: &[AgentUsageDimensionRow],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let header = h_flex()
            .h(px(34.0))
            .w_full()
            .flex_none()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.22))
            .child(table_label_header(
                dimension_label(self.request.dimension),
                cx,
            ))
            .child(self.render_sort_header(
                locale::text("Requests", "请求", "請求"),
                AgentUsageSortMetric::Requests,
                84.0,
                cx,
            ))
            .child(self.render_sort_header(
                locale::text("Total", "总量", "總量"),
                AgentUsageSortMetric::TotalTokens,
                108.0,
                cx,
            ))
            .child(self.render_sort_header(
                locale::text("Input", "输入", "輸入"),
                AgentUsageSortMetric::InputTokens,
                100.0,
                cx,
            ))
            .child(self.render_sort_header(
                locale::text("Output", "输出", "輸出"),
                AgentUsageSortMetric::OutputTokens,
                100.0,
                cx,
            ))
            .child(self.render_sort_header(
                locale::text("Cache", "缓存", "快取"),
                AgentUsageSortMetric::CachedTokens,
                100.0,
                cx,
            ))
            .child(self.render_sort_header(
                locale::text("Hit rate", "命中率", "命中率"),
                AgentUsageSortMetric::CacheHitRate,
                92.0,
                cx,
            ))
            .child(self.render_sort_header(
                locale::text("Last activity", "最近活动", "最近活動"),
                AgentUsageSortMetric::LastActivity,
                126.0,
                cx,
            ))
            .child(table_plain_header(
                locale::text("Coverage", "上报覆盖", "回報覆蓋"),
                118.0,
                cx,
            ));
        let mut body = v_flex().w_full();
        if rows.is_empty() {
            body = body.child(
                div()
                    .h(px(96.0))
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(locale::text(
                        "No usage facts match these filters",
                        "没有符合筛选条件的用量记录",
                        "沒有符合篩選條件的用量記錄",
                    )),
            );
        } else {
            for (index, row) in rows.iter().enumerate() {
                body = body.child(render_table_row(index, row, cx));
            }
        }
        let wheel_scroll = self.table_scroll.clone();
        div()
            .id("usage-table-scroll")
            .w_full()
            .overflow_x_scroll()
            .track_scroll(&self.table_scroll)
            .on_scroll_wheel(cx.listener(move |_, event: &ScrollWheelEvent, window, cx| {
                let max_x = wheel_scroll.max_offset().x;
                if max_x <= px(0.0) {
                    return;
                }
                let delta = event.delta.pixel_delta(window.line_height());
                if delta.y.abs() > delta.x.abs() {
                    let offset = wheel_scroll.offset();
                    // GPUI applies delta.x before bubble listeners run.
                    let next_x = (offset.x - delta.x + delta.y).clamp(-max_x, px(0.0));
                    if next_x != offset.x {
                        wheel_scroll.set_offset(point(next_x, offset.y));
                        cx.notify();
                    }
                }
                cx.stop_propagation();
            }))
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(6.0))
            .child(
                v_flex()
                    .min_w(px(USAGE_TABLE_MIN_WIDTH))
                    .w_full()
                    .child(header)
                    .child(body),
            )
            .into_any_element()
    }

    fn render_sort_header(
        &mut self,
        label: &'static str,
        metric: AgentUsageSortMetric,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.request.sort_metric == metric;
        let ascending = self.request.sort_direction == AgentUsageSortDirection::Ascending;
        Button::new(SharedString::from(format!("usage-sort-{metric:?}")))
            .xsmall()
            .ghost()
            .h_full()
            .w(px(width))
            .justify_end()
            .label(label)
            .when(active, |button| {
                button.icon(if ascending {
                    IconName::ArrowUp
                } else {
                    IconName::ArrowDown
                })
            })
            .on_click(cx.listener(move |this, _, _, cx| this.choose_sort(metric, cx)))
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

impl Render for UsageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport_width = f32::from(window.viewport_size().width);
        let header = self.render_header(cx);
        let status = self.render_status(cx);
        let statistics = self.statistics.clone();
        let state = usage_content_state(
            statistics
                .as_ref()
                .map(|statistics| statistics.totals.requests),
            self.loading,
            self.error.is_some(),
        );
        let content = match state {
            UsageContentState::Ready => {
                let statistics = statistics.expect("ready usage state requires statistics");
                v_flex()
                    .w_full()
                    .gap_4()
                    .children(status)
                    .child(self.render_toolbar(cx))
                    .child(self.render_summary(&statistics.totals, viewport_width, cx))
                    .child(self.render_trend(&statistics, cx))
                    .child(self.render_dimensions(&statistics, cx))
                    .into_any_element()
            }
            UsageContentState::Loading => div()
                .h(px(240.0))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(locale::text(
                    "Loading usage statistics...",
                    "正在加载用量统计...",
                    "正在載入用量統計...",
                ))
                .into_any_element(),
            UsageContentState::Empty => v_flex()
                .w_full()
                .gap_4()
                .children(status)
                .child(self.render_toolbar(cx))
                .child(
                    div()
                        .h(px(180.0))
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
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
                .child(self.render_toolbar(cx))
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
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scrollbar()
                    .p_4()
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
        .h(px(72.0))
        .justify_center()
        .gap_1()
        .rounded(px(8.0))
        .border_1()
        .border_color(cx.theme().border.opacity(0.78))
        .bg(theme::semantic_color("card", cx.theme().is_dark()))
        .px_3()
        .child(
            h_flex()
                .min_w_0()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    Icon::new(icon)
                        .size(px(13.0))
                        .text_color(cx.theme().primary.opacity(0.82)),
                )
                .child(div().min_w_0().truncate().child(label)),
        )
        .child(
            div()
                .truncate()
                .text_lg()
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

fn nice_axis_upper(value: u64, minimum: u64) -> u64 {
    if value == 0 {
        return minimum;
    }
    let raw_step = value as f64 / 4.0;
    let magnitude = 10_f64.powf(raw_step.log10().floor());
    let normalized = raw_step / magnitude;
    let nice = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 2.5 {
        2.5
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    ((nice * magnitude * 4.0).ceil() as u64).max(minimum)
}

fn axis_ticks(maximum: u64) -> [u64; 5] {
    let step = maximum / 4;
    [
        maximum,
        step.saturating_mul(3),
        step.saturating_mul(2),
        step,
        0,
    ]
}

fn format_token_axis_k(value: u64) -> String {
    if value.is_multiple_of(1_000) {
        format!("{}K", value / 1_000)
    } else {
        let formatted = format!("{:.2}", value as f64 / 1_000.0);
        format!("{}K", formatted.trim_end_matches('0').trim_end_matches('.'))
    }
}

fn render_trend_axis(maximum: u64, width: f32, cx: &mut Context<UsageView>) -> AnyElement {
    let mut axis = v_flex()
        .h(px(USAGE_CHART_HEIGHT))
        .w(px(width))
        .flex_none()
        .justify_between()
        .text_size(px(10.0))
        .text_color(cx.theme().muted_foreground);
    for tick in axis_ticks(maximum) {
        axis = axis.child(
            div()
                .w_full()
                .pr_2()
                .text_right()
                .child(format_token_axis_k(tick)),
        );
    }
    axis.into_any_element()
}

fn stacked_bucket_total(aggregate: &AgentUsageAggregate, metrics: &[AgentUsageTrendMetric]) -> u64 {
    metrics.iter().fold(0_u64, |total, metric| {
        total.saturating_add(trend_value(aggregate, *metric).unwrap_or(0))
    })
}

fn paint_chart_rect(
    window: &mut Window,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    color: Hsla,
) {
    window.paint_quad(quad(
        Bounds::from_corners(point(px(left), px(top)), point(px(right), px(bottom))),
        px(1.0),
        color,
        Edges::default(),
        transparent_black(),
        BorderStyle::default(),
    ));
}

fn render_stacked_trend(
    statistics: &AgentUsageStatistics,
    enabled_metrics: &[AgentUsageTrendMetric],
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let bar_data = usage_trend_series(cx)
        .into_iter()
        .filter(|series| enabled_metrics.contains(&series.metric))
        .map(|series| {
            (
                series.color,
                statistics
                    .trend_buckets
                    .iter()
                    .map(|bucket| trend_value(&bucket.aggregate, series.metric))
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let stack_maximum = statistics
        .trend_buckets
        .iter()
        .map(|bucket| stacked_bucket_total(&bucket.aggregate, enabled_metrics))
        .max()
        .unwrap_or(0);
    let token_axis_maximum = nice_axis_upper(stack_maximum, 1_000);
    let has_values = bar_data
        .iter()
        .flat_map(|(_, values)| values.iter().flatten())
        .any(|value| *value > 0);
    let has_metrics = !bar_data.is_empty();
    let bucket_count = statistics.trend_buckets.len();

    let mut grid = v_flex().absolute().inset_0().justify_between();
    for _ in 0..5 {
        grid = grid.child(
            div()
                .w_full()
                .border_t_1()
                .border_color(cx.theme().border.opacity(0.42)),
        );
    }
    let mut hitboxes = h_flex().absolute().inset_0();
    for (index, bucket) in statistics.trend_buckets.iter().enumerate() {
        let tooltip_title = bucket.label.clone();
        let tooltip_rows = trend_bucket_tooltip_rows(&bucket.aggregate, enabled_metrics);
        hitboxes = hitboxes.child(
            div()
                .id(SharedString::from(format!("usage-trend-bucket-{index}")))
                .h_full()
                .min_w(px(2.0))
                .flex_1()
                .tooltip(move |window, cx| {
                    let title = tooltip_title.clone();
                    let rows = tooltip_rows.clone();
                    Tooltip::element(move |_, cx| {
                        let mut content = v_flex()
                            .min_w(px(190.0))
                            .gap_1()
                            .text_xs()
                            .child(div().font_medium().child(title.clone()));
                        for (label, value) in rows.iter() {
                            content = content.child(
                                h_flex()
                                    .w_full()
                                    .justify_between()
                                    .gap_4()
                                    .child(
                                        div()
                                            .text_color(cx.theme().popover_foreground.opacity(0.72))
                                            .child(*label),
                                    )
                                    .child(div().font_medium().child(value.clone())),
                            );
                        }
                        content
                    })
                    .build(window, cx)
                }),
        );
    }
    let plot = div()
        .relative()
        .h(px(USAGE_CHART_HEIGHT))
        .min_w_0()
        .flex_1()
        .overflow_hidden()
        .border_b_1()
        .border_color(cx.theme().border.opacity(0.55))
        .child(grid)
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let width = f32::from(bounds.size.width).max(1.0);
                    let height = f32::from(bounds.size.height).max(1.0);
                    let bucket_count = bucket_count.max(1);
                    let bucket_width = width / bucket_count as f32;
                    let bar_width = (bucket_width * 0.64).clamp(2.0, 32.0);
                    let origin_x = f32::from(bounds.origin.x);
                    let origin_y = f32::from(bounds.origin.y);
                    for index in 0..bucket_count {
                        let center = origin_x + bucket_width * (index as f32 + 0.5);
                        let mut bottom = origin_y + height - 2.0;
                        for (color, values) in &bar_data {
                            let Some(value) = values.get(index).copied().flatten() else {
                                continue;
                            };
                            if value == 0 {
                                continue;
                            }
                            let segment_height = (value.min(token_axis_maximum) as f64
                                / token_axis_maximum.max(1) as f64
                                * f64::from((height - 4.0).max(1.0)))
                                as f32;
                            let top = bottom - segment_height;
                            paint_chart_rect(
                                window,
                                center - bar_width / 2.0,
                                top,
                                center + bar_width / 2.0,
                                bottom,
                                *color,
                            );
                            bottom = top;
                        }
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .child(hitboxes)
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
        });

    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .child(render_trend_axis(
                    token_axis_maximum,
                    USAGE_CHART_AXIS_WIDTH,
                    cx,
                ))
                .child(plot),
        )
        .child(
            h_flex()
                .w_full()
                .child(div().w(px(USAGE_CHART_AXIS_WIDTH)).flex_none())
                .child(render_trend_labels(statistics, cx)),
        )
        .into_any_element()
}

fn trend_bucket_tooltip_rows(
    aggregate: &AgentUsageAggregate,
    enabled_metrics: &[AgentUsageTrendMetric],
) -> Vec<(&'static str, String)> {
    let display = |value: Option<u64>| {
        value
            .map(format_full_number)
            .unwrap_or_else(|| locale::text("Unknown", "未知", "未知").to_string())
    };
    enabled_metrics
        .iter()
        .map(|metric| {
            let label = match metric {
                AgentUsageTrendMetric::Requests => locale::text("Requests", "请求", "請求"),
                AgentUsageTrendMetric::TotalTokens => locale::text("Total", "总量", "總量"),
                AgentUsageTrendMetric::InputTokens => locale::text("Input", "输入", "輸入"),
                AgentUsageTrendMetric::OutputTokens => locale::text("Output", "输出", "輸出"),
                AgentUsageTrendMetric::CachedTokens => locale::text("Cache", "缓存", "快取"),
            };
            (label, display(trend_value(aggregate, *metric)))
        })
        .collect()
}

fn render_trend_labels(
    statistics: &AgentUsageStatistics,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let count = statistics.trend_buckets.len();
    let mut labels = h_flex().w_full();
    for (index, bucket) in statistics.trend_buckets.iter().enumerate() {
        let visible = count <= 8 || index == 0 || index + 1 == count || index % 6 == 0;
        labels = labels.child(
            div()
                .min_w(px(2.0))
                .flex_1()
                .text_center()
                .text_size(px(10.0))
                .text_color(cx.theme().muted_foreground)
                .child(if visible {
                    bucket.label.clone()
                } else {
                    String::new()
                }),
        );
    }
    labels.into_any_element()
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

fn render_percentage_axis(cx: &mut Context<UsageView>) -> AnyElement {
    v_flex()
        .h(px(USAGE_CHART_HEIGHT))
        .w(px(38.0))
        .flex_none()
        .justify_between()
        .text_size(px(10.0))
        .text_color(cx.theme().muted_foreground)
        .children(
            ["100%", "50%", "0%"].map(|label| div().w_full().text_right().pr_2().child(label)),
        )
        .into_any_element()
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
    let day_values = annual
        .days
        .iter()
        .map(|day| {
            let values = categories
                .iter()
                .map(|category| model_day_value(day, category, metric, &visible_model_ids))
                .collect::<Vec<_>>();
            (day.label.clone(), values)
        })
        .collect::<Vec<_>>();
    let mut grid = v_flex().absolute().inset_0().justify_between();
    for _ in 0..3 {
        grid = grid.child(
            div()
                .w_full()
                .border_t_1()
                .border_color(cx.theme().border.opacity(0.42)),
        );
    }
    let mut hitboxes = h_flex().absolute().inset_0();
    for (index, (label, values)) in day_values.iter().enumerate() {
        let title = label.clone();
        let denominator = values
            .iter()
            .flatten()
            .fold(0_u64, |sum, value| sum.saturating_add(*value));
        let rows = categories
            .iter()
            .zip(values.iter())
            .map(|(category, value)| {
                (
                    category.label.clone(),
                    value.map_or_else(
                        || locale::text("Unknown", "未知", "未知").to_string(),
                        |value| {
                            let percentage = if denominator == 0 {
                                0.0
                            } else {
                                value as f64 / denominator as f64 * 100.0
                            };
                            format!("{} · {percentage:.1}%", format_full_number(value))
                        },
                    ),
                )
            })
            .collect::<Vec<_>>();
        hitboxes = hitboxes.child(
            div()
                .id(SharedString::from(format!("usage-model-day-{index}")))
                .h_full()
                .min_w(px(1.0))
                .flex_1()
                .tooltip(move |window, cx| {
                    let title = title.clone();
                    let rows = rows.clone();
                    Tooltip::element(move |_, cx| {
                        let mut content = v_flex()
                            .min_w(px(190.0))
                            .gap_1()
                            .text_xs()
                            .child(div().font_medium().child(title.clone()));
                        for (label, value) in &rows {
                            content = content.child(
                                h_flex()
                                    .w_full()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .text_color(cx.theme().popover_foreground.opacity(0.72))
                                            .child(label.clone()),
                                    )
                                    .child(div().font_medium().child(value.clone())),
                            );
                        }
                        content.child(
                            div()
                                .text_color(cx.theme().popover_foreground.opacity(0.62))
                                .child(model_metric_label(metric)),
                        )
                    })
                    .build(window, cx)
                }),
        );
    }
    let chart_data = day_values
        .iter()
        .map(|(_, values)| values.clone())
        .collect::<Vec<_>>();
    let colors = categories
        .iter()
        .map(|category| category.color)
        .collect::<Vec<_>>();
    let plot = div()
        .relative()
        .h(px(USAGE_CHART_HEIGHT))
        .min_w_0()
        .flex_1()
        .overflow_hidden()
        .child(grid)
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let width = f32::from(bounds.size.width).max(1.0);
                    let height = f32::from(bounds.size.height).max(1.0);
                    let count = chart_data.len().max(1);
                    let day_width = width / count as f32;
                    let bar_width = (day_width * 0.82).clamp(1.0, 8.0);
                    let origin_x = f32::from(bounds.origin.x);
                    let origin_y = f32::from(bounds.origin.y);
                    for (index, values) in chart_data.iter().enumerate() {
                        let total = values
                            .iter()
                            .flatten()
                            .fold(0_u64, |sum, value| sum.saturating_add(*value));
                        if total == 0 {
                            continue;
                        }
                        let center = origin_x + day_width * (index as f32 + 0.5);
                        let mut bottom = origin_y + height;
                        for (value, color) in values.iter().zip(colors.iter()) {
                            let Some(value) = value else { continue };
                            if *value == 0 {
                                continue;
                            }
                            let segment_height =
                                (*value as f64 / total as f64 * height as f64) as f32;
                            let top = bottom - segment_height;
                            paint_chart_rect(
                                window,
                                center - bar_width / 2.0,
                                top,
                                center + bar_width / 2.0,
                                bottom,
                                *color,
                            );
                            bottom = top;
                        }
                    }
                },
            )
            .absolute()
            .inset_0(),
        )
        .child(hitboxes);
    let mut month_labels = h_flex().w_full().gap_0();
    let mut previous_month = None;
    for (label, _) in &day_values {
        let month = NaiveDate::parse_from_str(label, "%Y-%m-%d")
            .ok()
            .map(|date| date.month());
        let visible = month.is_some() && month != previous_month;
        if visible {
            previous_month = month;
        }
        month_labels = month_labels.child(
            div()
                .min_w(px(1.0))
                .flex_1()
                .text_size(px(10.0))
                .text_color(cx.theme().muted_foreground)
                .child(if visible {
                    month.map(month_label).unwrap_or_default()
                } else {
                    String::new()
                }),
        );
    }
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
    v_flex()
        .w_full()
        .gap_2()
        .child(
            div()
                .id("usage-model-chart-scroll")
                .w_full()
                .overflow_x_scroll()
                .child(
                    v_flex()
                        .min_w(px(USAGE_MODEL_CHART_MIN_WIDTH))
                        .gap_2()
                        .child(
                            h_flex()
                                .w_full()
                                .child(render_percentage_axis(cx))
                                .child(plot),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .child(div().w(px(38.0)).flex_none())
                                .child(month_labels),
                        ),
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

fn table_label_header(label: &'static str, cx: &mut Context<UsageView>) -> AnyElement {
    div()
        .min_w(px(210.0))
        .flex_1()
        .truncate()
        .px_3()
        .text_xs()
        .font_medium()
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .into_any_element()
}

fn table_plain_header(label: &'static str, width: f32, cx: &mut Context<UsageView>) -> AnyElement {
    div()
        .w(px(width))
        .flex_none()
        .truncate()
        .px_2()
        .text_right()
        .text_xs()
        .font_medium()
        .text_color(cx.theme().muted_foreground)
        .child(label)
        .into_any_element()
}

fn render_table_row(
    index: usize,
    row: &AgentUsageDimensionRow,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let label_tooltip = row.label.clone();
    h_flex()
        .id(SharedString::from(format!("usage-row-{index}")))
        .min_h(px(42.0))
        .w_full()
        .items_center()
        .border_b_1()
        .border_color(cx.theme().border.opacity(0.55))
        .when(index % 2 == 1, |this| {
            this.bg(cx.theme().muted.opacity(0.10))
        })
        .child(
            div()
                .id(SharedString::from(format!("usage-row-{index}-label")))
                .min_w(px(210.0))
                .flex_1()
                .truncate()
                .px_3()
                .text_sm()
                .font_medium()
                .child(row.label.clone())
                .tooltip(move |window, cx| Tooltip::new(label_tooltip.clone()).build(window, cx)),
        )
        .child(table_value(
            SharedString::from(format!("usage-row-{index}-requests")),
            format_compact_number(row.aggregate.requests),
            84.0,
            false,
            Some(format!(
                "{}: {}. {}",
                locale::text("Requests", "请求", "請求"),
                format_full_number(row.aggregate.requests),
                locale::text(
                    "Dispatched prompt executions",
                    "实际发送的 prompt 执行数",
                    "實際傳送的 prompt 執行數",
                )
            )),
            cx,
        ))
        .child(table_metric(
            index,
            "total",
            locale::text("Total tokens", "总 Token", "總 Token"),
            &row.aggregate.total_tokens,
            108.0,
            cx,
        ))
        .child(table_metric(
            index,
            "input",
            locale::text("Input tokens", "输入 Token", "輸入 Token"),
            &row.aggregate.input_tokens,
            100.0,
            cx,
        ))
        .child(table_metric(
            index,
            "output",
            locale::text("Output tokens", "输出 Token", "輸出 Token"),
            &row.aggregate.output_tokens,
            100.0,
            cx,
        ))
        .child(table_metric(
            index,
            "cached-read",
            locale::text("Cached read tokens", "缓存读取 Token", "快取讀取 Token"),
            &row.aggregate.cached_tokens,
            100.0,
            cx,
        ))
        .child(table_value(
            SharedString::from(format!("usage-row-{index}-cache-hit")),
            format_basis_points(row.aggregate.cache_hit_rate.basis_points),
            92.0,
            row.aggregate.cache_hit_rate.basis_points.is_none(),
            Some(format!(
                "{}: {}. {}",
                locale::text("Cache hit rate", "缓存命中率", "快取命中率"),
                format_basis_points(row.aggregate.cache_hit_rate.basis_points),
                cache_hit_detail(&row.aggregate.cache_hit_rate)
            )),
            cx,
        ))
        .child(table_value(
            SharedString::from(format!("usage-row-{index}-last-activity")),
            row.aggregate
                .last_activity_at_ms
                .map(format_timestamp)
                .unwrap_or_else(|| "-".to_string()),
            126.0,
            row.aggregate.last_activity_at_ms.is_none(),
            row.aggregate.last_activity_at_ms.map(|timestamp| {
                format!(
                    "{}: {}",
                    locale::text("Last activity", "最近活动", "最近活動"),
                    format_timestamp(timestamp)
                )
            }),
            cx,
        ))
        .child(table_coverage(index, &row.aggregate, 118.0, cx))
        .into_any_element()
}

fn table_metric(
    row_index: usize,
    id: &'static str,
    label: &'static str,
    metric: &AgentUsageMetricValue,
    width: f32,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    table_value(
        SharedString::from(format!("usage-row-{row_index}-{id}")),
        metric
            .value
            .map(format_compact_number)
            .unwrap_or_else(|| "-".to_string()),
        width,
        metric.value.is_none(),
        Some(format!(
            "{label}: {}. {}",
            metric_full_value(metric),
            metric_detail(metric)
        )),
        cx,
    )
}

fn table_value(
    id: SharedString,
    value: String,
    width: f32,
    unknown: bool,
    tooltip: Option<String>,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let element = div()
        .id(id)
        .w(px(width))
        .flex_none()
        .truncate()
        .px_2()
        .text_right()
        .text_xs()
        .when(unknown, |this| this.text_color(cx.theme().muted_foreground))
        .child(value);
    match tooltip {
        Some(tooltip) => element
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .into_any_element(),
        None => element.into_any_element(),
    }
}

fn table_coverage(
    row_index: usize,
    aggregate: &AgentUsageAggregate,
    width: f32,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let coverage = &aggregate.coverage;
    let tooltip = coverage_detail(aggregate);
    div()
        .id(SharedString::from(format!(
            "usage-row-{row_index}-coverage"
        )))
        .w(px(width))
        .flex_none()
        .truncate()
        .px_2()
        .text_right()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(coverage_compact_label(coverage))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .into_any_element()
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

fn format_full_number(value: u64) -> String {
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
    fn trend_axis_uses_a_stable_token_scale() {
        assert_eq!(nice_axis_upper(0, 1_000), 1_000);
        assert_eq!(nice_axis_upper(86_700, 1_000), 100_000);
        assert_eq!(axis_ticks(100_000), [100_000, 75_000, 50_000, 25_000, 0]);
        assert_eq!(format_token_axis_k(25_000), "25K");
        assert_eq!(format_token_axis_k(1_500), "1.5K");
        assert_eq!(format_token_axis_k(750), "0.75K");
        assert_eq!(format_token_axis_k(250), "0.25K");
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

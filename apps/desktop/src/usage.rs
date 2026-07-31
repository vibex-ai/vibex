use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, Render, SharedString, Styled as _,
    Task, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    scroll::ScrollableElement as _,
    tooltip::Tooltip,
    v_flex,
};
use vibex_backend::BackendFacade;
use vibex_core::{
    AgentId, AgentUsageAggregate, AgentUsageDimension, AgentUsageDimensionRow,
    AgentUsageFilterOption, AgentUsageMetricCoverage, AgentUsageMetricValue, AgentUsageRange,
    AgentUsageSortDirection, AgentUsageSortMetric, AgentUsageStatistics,
    AgentUsageStatisticsRequest, AgentUsageTrendMetric, ProjectId, ProviderProfileId,
    VibexSessionId,
};

use crate::locale;

const USAGE_HEADER_HEIGHT: f32 = 48.0;
const USAGE_CHART_HEIGHT: f32 = 176.0;
const USAGE_TABLE_MIN_WIDTH: f32 = 1040.0;

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

pub struct UsageView {
    backend: Option<BackendFacade>,
    request: AgentUsageStatisticsRequest,
    statistics: Option<AgentUsageStatistics>,
    loading: bool,
    stale: bool,
    error: Option<(String, String)>,
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

    fn choose_trend(&mut self, metric: AgentUsageTrendMetric, cx: &mut Context<Self>) {
        if self.request.trend_metric != metric {
            self.request.trend_metric = metric;
            self.refresh(cx);
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
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(6.0))
            .overflow_hidden()
            .children([
                summary_metric(
                    "requests",
                    locale::text("Requests", "请求数", "請求數"),
                    format_compact_number(aggregate.requests),
                    format_full_number(aggregate.requests),
                    locale::text(
                        "Dispatched prompt executions",
                        "实际发送的 prompt 执行数",
                        "實際傳送的 prompt 執行數",
                    ),
                    false,
                    cx,
                ),
                summary_metric_value(
                    "total",
                    locale::text("Total tokens", "总 Token", "總 Token"),
                    &aggregate.total_tokens,
                    cx,
                ),
                summary_metric_value(
                    "input",
                    locale::text("Input", "输入", "輸入"),
                    &aggregate.input_tokens,
                    cx,
                ),
                summary_metric_value(
                    "output",
                    locale::text("Output", "输出", "輸出"),
                    &aggregate.output_tokens,
                    cx,
                ),
                summary_metric_value(
                    "cached",
                    locale::text("Cached read", "缓存读取", "快取讀取"),
                    &aggregate.cached_tokens,
                    cx,
                ),
                summary_metric(
                    "cache-hit",
                    locale::text("Cache hit rate", "缓存命中率", "快取命中率"),
                    format_basis_points(aggregate.cache_hit_rate.basis_points),
                    format_basis_points(aggregate.cache_hit_rate.basis_points),
                    cache_hit_detail(&aggregate.cache_hit_rate),
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
        let metrics = [
            (
                AgentUsageTrendMetric::Requests,
                locale::text("Requests", "请求", "請求"),
            ),
            (
                AgentUsageTrendMetric::TotalTokens,
                locale::text("Total", "总量", "總量"),
            ),
            (
                AgentUsageTrendMetric::InputTokens,
                locale::text("Input", "输入", "輸入"),
            ),
            (
                AgentUsageTrendMetric::OutputTokens,
                locale::text("Output", "输出", "輸出"),
            ),
            (
                AgentUsageTrendMetric::CachedTokens,
                locale::text("Cache", "缓存", "快取"),
            ),
        ];
        let selected = self.request.trend_metric;
        let mut controls = h_flex().flex_wrap().gap_1();
        for (metric, label) in metrics {
            controls = controls.child(
                Button::new(SharedString::from(format!("usage-trend-{metric:?}")))
                    .xsmall()
                    .ghost()
                    .h(px(28.0))
                    .selected(metric == selected)
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_trend(metric, cx))),
            );
        }
        let values = statistics
            .trend_buckets
            .iter()
            .map(|bucket| trend_value(&bucket.aggregate, selected))
            .collect::<Vec<_>>();
        let maximum = values.iter().flatten().copied().max().unwrap_or(0);
        let has_values = values.iter().flatten().any(|value| *value > 0);
        let bucket_count = statistics.trend_buckets.len();
        let mut bars = h_flex()
            .h(px(USAGE_CHART_HEIGHT - 34.0))
            .w_full()
            .items_end()
            .gap(px(if bucket_count > 20 { 3.0 } else { 8.0 }));
        for (index, bucket) in statistics.trend_buckets.iter().enumerate() {
            let value = values[index];
            let height = match (value, maximum) {
                (Some(value), maximum) if maximum > 0 => {
                    5.0 + (value as f64 / maximum as f64 * 104.0) as f32
                }
                (Some(_), _) => 3.0,
                (None, _) => 0.0,
            };
            let tooltip = format!(
                "{}: {}",
                bucket.label,
                value
                    .map(format_full_number)
                    .unwrap_or_else(|| { locale::text("Unknown", "未知", "未知").to_string() })
            );
            bars =
                bars.child(
                    div()
                        .id(SharedString::from(format!("usage-trend-bar-{index}")))
                        .h_full()
                        .min_w(px(2.0))
                        .flex_1()
                        .flex()
                        .items_end()
                        .child(div().w_full().h(px(height)).rounded_t(px(2.0)).bg(
                            if value.is_some() {
                                cx.theme().primary.opacity(0.72)
                            } else {
                                cx.theme().muted.opacity(0.35)
                            },
                        ))
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)),
                );
        }
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
                    .child(div().text_sm().font_semibold().child(locale::text(
                        "Usage trend",
                        "用量趋势",
                        "用量趨勢",
                    )))
                    .child(controls),
            )
            .child(
                div()
                    .relative()
                    .h(px(USAGE_CHART_HEIGHT))
                    .w_full()
                    .border_b_1()
                    .border_color(cx.theme().border.opacity(0.55))
                    .child(bars)
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
                                .child(locale::text(
                                    "No reported values in this range",
                                    "此范围内没有已上报数值",
                                    "此範圍內沒有已回報數值",
                                )),
                        )
                    }),
            )
            .child(render_trend_labels(statistics, cx))
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
        div()
            .id("usage-table-scroll")
            .w_full()
            .max_h(px(420.0))
            .overflow_x_scroll()
            .overflow_y_scrollbar()
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
        let statistics = self.statistics.as_ref()?;
        let coverage = &statistics.totals.coverage;
        let incomplete = coverage
            .partial_requests
            .saturating_add(coverage.baseline_only_requests)
            .saturating_add(coverage.unreported_requests)
            .saturating_add(coverage.unsupported_requests);
        if incomplete == 0 && !self.stale {
            return None;
        }
        let text = if self.stale {
            locale::text(
                "Refreshing after new usage was committed",
                "新用量已提交，正在刷新",
                "新用量已提交，正在重新整理",
            )
            .to_string()
        } else {
            match locale::current_locale() {
                locale::ResolvedLocale::En => format!(
                    "Partial reporting for {incomplete} of {} requests",
                    coverage.total_requests
                ),
                locale::ResolvedLocale::ZhCn => format!(
                    "{} 个请求中有 {incomplete} 个上报不完整",
                    coverage.total_requests
                ),
                locale::ResolvedLocale::ZhTw => format!(
                    "{} 個請求中有 {incomplete} 個回報不完整",
                    coverage.total_requests
                ),
            }
        };
        Some(
            h_flex()
                .w_full()
                .min_h(px(34.0))
                .items_center()
                .gap_2()
                .rounded(px(6.0))
                .bg(cx.theme().muted.opacity(0.28))
                .px_3()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(text)
                .into_any_element(),
        )
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
    metric: &AgentUsageMetricValue,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let display_value = metric
        .value
        .map(format_compact_number)
        .unwrap_or_else(|| locale::text("Unknown", "未知", "未知").to_string());
    let full_value = metric
        .value
        .map(format_full_number)
        .unwrap_or_else(|| locale::text("Unknown", "未知", "未知").to_string());
    summary_metric(
        id,
        label,
        display_value,
        full_value,
        metric_detail(metric),
        metric.value.is_none(),
        cx,
    )
}

fn summary_metric(
    id: &'static str,
    label: &'static str,
    value: String,
    full_value: String,
    detail: impl Into<String>,
    unknown: bool,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let detail = detail.into();
    let tooltip = format!("{label}: {full_value}. {detail}");
    v_flex()
        .id(SharedString::from(format!("usage-summary-{id}")))
        .min_w_0()
        .h(px(84.0))
        .justify_center()
        .gap_1()
        .border_r_1()
        .border_b_1()
        .border_color(cx.theme().border.opacity(0.65))
        .px_3()
        .child(
            div()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .truncate()
                .text_lg()
                .font_semibold()
                .when(unknown, |this| this.text_color(cx.theme().muted_foreground))
                .child(value),
        )
        .child(
            div()
                .truncate()
                .text_size(px(11.0))
                .text_color(cx.theme().muted_foreground.opacity(0.82))
                .child(detail),
        )
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
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
        AgentUsageTrendMetric::Requests => Some(aggregate.requests),
        AgentUsageTrendMetric::TotalTokens => aggregate.total_tokens.value,
        AgentUsageTrendMetric::InputTokens => aggregate.input_tokens.value,
        AgentUsageTrendMetric::OutputTokens => aggregate.output_tokens.value,
        AgentUsageTrendMetric::CachedTokens => aggregate.cached_tokens.value,
    }
}

fn render_trend_labels(
    statistics: &AgentUsageStatistics,
    cx: &mut Context<UsageView>,
) -> AnyElement {
    let count = statistics.trend_buckets.len();
    let mut labels = h_flex()
        .w_full()
        .gap(px(if count > 20 { 3.0 } else { 8.0 }));
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
        Some(0) => UsageContentState::Empty,
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
    fn successful_zero_request_query_uses_enablement_empty_state() {
        assert_eq!(
            usage_content_state(Some(0), false, false),
            UsageContentState::Empty
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
}

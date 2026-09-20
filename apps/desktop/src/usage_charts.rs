//! gpui-component `Plot` implementations for the Usage Statistics charts.
//!
//! Both Usage charts are stacked columns. gpui-component's `BarChart` draws a
//! single series per band, so the library's own answer for a stack is a `Plot`
//! built from its primitives — which is what this module does: `Stack` + `Bar`
//! for the marks, `ScaleBand`/`ScaleLinear` for the mapping, `PlotAxis` and
//! `Grid` for the frame, and the built-in hover tooltip for inspection. The
//! component owns every painted pixel; this module only maps Usage data onto
//! those primitives.

use gpui::{
    AnyElement, App, Bounds, Corners, ElementId, Hsla, IntoElement, Pixels, Point, SharedString,
    TextAlign, Window, point, px, size,
};
use gpui_base::{Spring, spring};
use gpui_component::{
    ActiveTheme as _,
    plot::{
        AXIS_GAP, AxisText, Grid, IntoPlot, Plot, PlotAxis,
        label::TEXT_GAP,
        scale::{Scale, ScaleBand, ScaleLinear},
        shape::{Bar, Stack, StackSeries},
        tooltip::{CrossLine, PlotHover, Tooltip, TooltipState},
    },
};

use crate::{locale, usage::format_full_number};

/// Left gutter the trend chart reserves for its K-unit Token axis.
const TREND_AXIS_GUTTER: f32 = 48.0;
/// Left gutter the model chart reserves for its 0–100% axis.
const MODEL_AXIS_GUTTER: f32 = 38.0;
/// Bars stop this far below the top of the plot, leaving the tallest one air.
const VALUE_HEADROOM: f32 = 10.0;
/// The trend axis always shows at least this much, so an empty range still
/// reads as a Token scale instead of a flat line.
const MIN_TOKEN_AXIS_MAXIMUM: u64 = 1_000;
/// Hairline radius shared by every column segment, matching the rest of the
/// workbench's data marks.
const SEGMENT_RADIUS: f32 = 1.0;

/// Where a chart's hover highlight is this frame.
///
/// `center` is the x the highlight band has reached, which trails the hovered
/// column while it travels. The band's fade is not tracked here: the tooltip
/// that draws it already reads the hover's focus from the plot's own hover
/// memory, so it eases in and out without being handed the value.
#[derive(Clone, Copy)]
struct ChartHover {
    center: Pixels,
}

/// The spring a chart's highlight band follows the hovered column with.
///
/// The kit's own pointer spring: the fast motion tier as a critically damped
/// response, with a sub-pixel tolerance so the band rests once nothing visible
/// moves. `gpui-base` adopts the target without animating when the system asks
/// for reduced motion, so this needs no separate guard.
fn pointer_spring(cx: &App) -> Spring {
    Spring::new(cx.theme().motion_tokens().duration_fast).with_epsilon(0.1)
}

/// Sample the highlight band's position for one hovered column.
///
/// `id` keys the spring within the plot's element scope. The first hovered
/// frame adopts the column instead of travelling from wherever the last hover
/// ended, which is what keeps a re-entry from sweeping the band across the
/// chart.
fn track_hover_band(
    id: (&'static str, &'static str),
    hover: &PlotHover,
    window: &mut Window,
    cx: &mut App,
) -> ChartHover {
    let policy = pointer_spring(cx).with_travel(!hover.is_entering());
    ChartHover {
        center: spring(id, hover.state().cross_line.x, policy, window, cx),
    }
}

/// One column of the trend chart: its band label plus one value slot per
/// enabled series. `None` marks a metric the adapters did not report for this
/// bucket, which must stay unknown rather than become zero.
#[derive(Clone)]
pub(crate) struct TrendChartBucket {
    label: SharedString,
    values: Vec<Option<u64>>,
}

impl TrendChartBucket {
    pub(crate) fn new(label: impl Into<SharedString>, values: Vec<Option<u64>>) -> Self {
        Self {
            label: label.into(),
            values,
        }
    }

    /// Whether any enabled series reported a positive value for this bucket.
    pub(crate) fn has_reported_value(&self) -> bool {
        self.values.iter().flatten().any(|value| *value > 0)
    }
}

/// One trend series with the label and color its legend entry already uses.
pub(crate) struct TrendChartSeries {
    label: SharedString,
    color: Hsla,
}

impl TrendChartSeries {
    pub(crate) fn new(label: impl Into<SharedString>, color: Hsla) -> Self {
        Self {
            label: label.into(),
            color,
        }
    }
}

/// The Usage trend: one stacked column per bucket over a left K-unit Token
/// axis, with a hover tooltip that keeps unknown values unknown.
#[derive(IntoPlot)]
pub(crate) struct TrendChart {
    buckets: Vec<TrendChartBucket>,
    series: Vec<TrendChartSeries>,
    stacked: Vec<StackSeries<TrendChartBucket>>,
    axis_maximum: u64,
    /// The highlight band's position this frame, sampled by `Plot::hover`.
    hover: Option<ChartHover>,
}

impl TrendChart {
    pub(crate) fn new(buckets: Vec<TrendChartBucket>, series: Vec<TrendChartSeries>) -> Self {
        let stacked = stack_series(&buckets, series.len(), |bucket, index| {
            bucket
                .values
                .get(index)
                .copied()
                .flatten()
                .map(|value| value as f32)
        });
        let stack_maximum = buckets
            .iter()
            .map(|bucket| {
                bucket
                    .values
                    .iter()
                    .flatten()
                    .fold(0_u64, |total, value| total.saturating_add(*value))
            })
            .max()
            .unwrap_or(0);
        Self {
            buckets,
            series,
            stacked,
            axis_maximum: nice_axis_upper(stack_maximum, MIN_TOKEN_AXIS_MAXIMUM),
            hover: None,
        }
    }

    /// Plot area and band scale, shared by `paint` and both tooltip hooks so
    /// the crosshair always lands on the band the bars were painted into.
    fn layout(&self, bounds: Bounds<Pixels>) -> TrendLayout {
        let plot_width = (bounds.size.width.as_f32() - TREND_AXIS_GUTTER).max(1.0);
        let plot_height = (bounds.size.height.as_f32() - AXIS_GAP).max(1.0);
        let plot_bounds = Bounds {
            origin: bounds.origin + point(px(TREND_AXIS_GUTTER), px(0.)),
            size: size(px(plot_width), px(plot_height)),
        };
        let x = ScaleBand::new(
            self.buckets
                .iter()
                .map(|bucket| bucket.label.clone())
                .collect(),
            vec![0., plot_width],
        )
        .padding_inner(0.4)
        .padding_outer(0.2);
        TrendLayout {
            plot_bounds,
            plot_height,
            band_width: x.band_width(),
            x,
        }
    }
}

struct TrendLayout {
    plot_bounds: Bounds<Pixels>,
    plot_height: f32,
    band_width: f32,
    x: ScaleBand<SharedString>,
}

impl Plot for TrendChart {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        if self.buckets.is_empty() || self.series.is_empty() {
            return;
        }
        let TrendLayout {
            plot_bounds,
            plot_height,
            band_width,
            x,
        } = self.layout(bounds);
        let y = ScaleLinear::new(
            vec![0., self.axis_maximum as f64],
            vec![plot_height, VALUE_HEADROOM],
        );

        // Left K-unit Token axis: the labels sit in the gutter the band scale
        // leaves clear, right-aligned against the plot area.
        PlotAxis::new()
            .y_axis(false)
            .y(px(TREND_AXIS_GUTTER - TEXT_GAP * 2.0))
            .y_label(
                axis_ticks(self.axis_maximum)
                    .into_iter()
                    .filter_map(|tick| {
                        let tick_y = y.tick(&(tick as f64))?;
                        Some(
                            AxisText::new(
                                format_token_axis_k(tick),
                                px(tick_y),
                                cx.theme().muted_foreground,
                            )
                            .align(TextAlign::Right),
                        )
                    }),
            )
            .paint(&bounds, window, cx);

        // The baseline grid line is the band axis itself, so the grid stops one
        // tick short of the floor.
        let ticks = axis_ticks(self.axis_maximum);
        let grid_lines = ticks[..ticks.len() - 1]
            .iter()
            .filter_map(|tick| y.tick(&(*tick as f64)))
            .collect::<Vec<_>>();
        Grid::new()
            .y(grid_lines)
            .stroke(cx.theme().border)
            .dash_array(&[px(4.), px(2.)])
            .paint(&plot_bounds, window);

        // Band axis with the bucket labels; dense ranges thin them so they
        // never overlap into a smear.
        let label_step = trend_label_step(self.buckets.len());
        PlotAxis::new()
            .x(px(plot_height))
            .x_label(
                self.buckets
                    .iter()
                    .enumerate()
                    .filter_map(|(index, bucket)| {
                        if !trend_label_visible(index, self.buckets.len(), label_step) {
                            return None;
                        }
                        let tick = x.tick(&bucket.label)? + band_width / 2.0;
                        Some(
                            AxisText::new(
                                bucket.label.clone(),
                                px(tick),
                                cx.theme().muted_foreground,
                            )
                            .align(TextAlign::Center),
                        )
                    }),
            )
            .stroke(cx.theme().border)
            .paint(&plot_bounds, window, cx);

        // One `Bar` per series; `Stack` supplies each segment's base and top,
        // so the segments tile the column exactly.
        for (series, stacked) in self.series.iter().zip(self.stacked.iter()) {
            let x = x.clone();
            let y0 = y.clone();
            let y1 = y.clone();
            let color = series.color;
            Bar::new()
                .data(&stacked.points)
                .band_width(band_width)
                .cross(move |point| x.tick(&point.data.label))
                .base(move |point| y0.tick(&(point.y0 as f64)).unwrap_or(plot_height))
                .value(move |point| y1.tick(&(point.y1 as f64)))
                .fill(move |_, _, _| color)
                .corner_radii(Corners::all(px(SEGMENT_RADIUS)))
                .paint(&plot_bounds, window, cx);
        }
    }

    fn id(&self) -> Option<ElementId> {
        Some("usage-trend-chart".into())
    }

    fn hover(&mut self, hover: Option<&PlotHover>, window: &mut Window, cx: &mut App) {
        self.hover =
            hover.map(|hover| track_hover_band(("usage-trend-chart", "band"), hover, window, cx));
    }

    fn tooltip_state(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _cx: &App,
    ) -> Option<TooltipState> {
        let TrendLayout {
            plot_height,
            band_width,
            x,
            ..
        } = self.layout(bounds);
        // Hovering the value axis or the band labels shows nothing.
        if position.x < px(TREND_AXIS_GUTTER) || position.y > px(plot_height) {
            return None;
        }
        let index = x.least_index(position.x.as_f32() - TREND_AXIS_GUTTER);
        let center =
            x.tick(&self.buckets.get(index)?.label)? + band_width / 2.0 + TREND_AXIS_GUTTER;
        Some(TooltipState::new(
            index,
            point(px(center), position.y),
            Vec::new(),
        ))
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<AnyElement> {
        let bucket = self.buckets.get(state.index)?;
        let TrendLayout {
            plot_height,
            band_width,
            ..
        } = self.layout(bounds);
        // The band is centered where the hover spring has reached rather than
        // snapped to the column, so it slides between buckets instead of
        // jumping. Before the first sample it stays on the hovered column.
        let center = self.hover.map_or(state.cross_line, |hover| {
            point(hover.center, state.cross_line.y)
        });
        let mut tooltip = Tooltip::new(cursor, bounds.size)
            .gap(px(8.0))
            .cross_line(
                CrossLine::new(center)
                    .span(0.0, plot_height)
                    .band(px(band_width)),
            )
            .title(bucket.label.clone());
        for (series, value) in self.series.iter().zip(bucket.values.iter()) {
            tooltip = tooltip.row(
                series.color,
                series.label.clone(),
                metric_value_text(*value),
            );
        }
        Some(tooltip.into_any_element())
    }
}

/// One column of the model chart: the day, its month-start label (if any), and
/// one raw value slot per category. The normalized shares the columns are
/// painted from stay private to the chart.
#[derive(Clone)]
pub(crate) struct ModelChartDay {
    label: SharedString,
    month_label: Option<SharedString>,
    values: Vec<Option<u64>>,
    shares: Vec<f32>,
}

impl ModelChartDay {
    pub(crate) fn new(
        label: impl Into<SharedString>,
        month_label: Option<SharedString>,
        values: Vec<Option<u64>>,
    ) -> Self {
        let shares = normalized_shares(&values);
        Self {
            label: label.into(),
            month_label,
            values,
            shares,
        }
    }
}

/// One model category with the label and color its legend entry already uses.
pub(crate) struct ModelChartCategory {
    label: SharedString,
    color: Hsla,
}

impl ModelChartCategory {
    pub(crate) fn new(label: impl Into<SharedString>, color: Hsla) -> Self {
        Self {
            label: label.into(),
            color,
        }
    }
}

/// The Usage model view: one 100%-normalized column per day, stacked by model
/// category, over the same 0–100% axis and hover tooltip the trend uses.
#[derive(IntoPlot)]
pub(crate) struct ModelChart {
    days: Vec<ModelChartDay>,
    categories: Vec<ModelChartCategory>,
    stacked: Vec<StackSeries<ModelChartDay>>,
    metric_label: SharedString,
    /// The highlight band's position this frame, sampled by `Plot::hover`.
    hover: Option<ChartHover>,
}

impl ModelChart {
    pub(crate) fn new(
        days: Vec<ModelChartDay>,
        categories: Vec<ModelChartCategory>,
        metric_label: impl Into<SharedString>,
    ) -> Self {
        let stacked = stack_series(&days, categories.len(), |day, index| {
            day.shares.get(index).copied()
        });
        Self {
            days,
            categories,
            stacked,
            metric_label: metric_label.into(),
            hover: None,
        }
    }

    fn layout(&self, bounds: Bounds<Pixels>) -> ModelLayout {
        let plot_width = (bounds.size.width.as_f32() - MODEL_AXIS_GUTTER).max(1.0);
        let plot_height = (bounds.size.height.as_f32() - AXIS_GAP).max(1.0);
        let plot_bounds = Bounds {
            origin: bounds.origin + point(px(MODEL_AXIS_GUTTER), px(0.)),
            size: size(px(plot_width), px(plot_height)),
        };
        let x = ScaleBand::new(
            self.days.iter().map(|day| day.label.clone()).collect(),
            vec![0., plot_width],
        )
        .padding_inner(0.3)
        .padding_outer(0.1);
        ModelLayout {
            plot_bounds,
            plot_height,
            band_width: x.band_width().max(1.0),
            x,
        }
    }
}

struct ModelLayout {
    plot_bounds: Bounds<Pixels>,
    plot_height: f32,
    band_width: f32,
    x: ScaleBand<SharedString>,
}

impl Plot for ModelChart {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        if self.days.is_empty() || self.categories.is_empty() {
            return;
        }
        let ModelLayout {
            plot_bounds,
            plot_height,
            band_width,
            x,
        } = self.layout(bounds);
        let y = ScaleLinear::new(vec![0., 1.], vec![plot_height, 0.]);

        // 0–100% axis in the left gutter.
        PlotAxis::new()
            .y_axis(false)
            .y(px(MODEL_AXIS_GUTTER - TEXT_GAP * 2.0))
            .y_label([1.0_f64, 0.5, 0.0].into_iter().filter_map(|share| {
                let tick = y.tick(&share)?;
                Some(
                    AxisText::new(
                        format_percentage_axis(share),
                        px(tick),
                        cx.theme().muted_foreground,
                    )
                    .align(TextAlign::Right),
                )
            }))
            .paint(&bounds, window, cx);

        // The floor grid line is the band axis itself.
        Grid::new()
            .y(vec![0.0, plot_height / 2.0])
            .stroke(cx.theme().border)
            .dash_array(&[px(4.), px(2.)])
            .paint(&plot_bounds, window);

        // Month starts label the band axis; every other day stays blank so the
        // labels read as calendar months rather than a date smear.
        PlotAxis::new()
            .x(px(plot_height))
            .x_label(self.days.iter().filter_map(|day| {
                let label = day.month_label.clone()?;
                let tick = x.tick(&day.label)? + band_width / 2.0;
                Some(
                    AxisText::new(label, px(tick), cx.theme().muted_foreground)
                        .align(TextAlign::Center),
                )
            }))
            .stroke(cx.theme().border)
            .paint(&plot_bounds, window, cx);

        for (category, stacked) in self.categories.iter().zip(self.stacked.iter()) {
            let x = x.clone();
            let y0 = y.clone();
            let y1 = y.clone();
            let color = category.color;
            Bar::new()
                .data(&stacked.points)
                .band_width(band_width)
                .cross(move |point| x.tick(&point.data.label))
                .base(move |point| y0.tick(&(point.y0 as f64)).unwrap_or(plot_height))
                .value(move |point| y1.tick(&(point.y1 as f64)))
                .fill(move |_, _, _| color)
                .corner_radii(Corners::all(px(SEGMENT_RADIUS)))
                .paint(&plot_bounds, window, cx);
        }
    }

    fn id(&self) -> Option<ElementId> {
        Some("usage-model-chart".into())
    }

    fn hover(&mut self, hover: Option<&PlotHover>, window: &mut Window, cx: &mut App) {
        self.hover =
            hover.map(|hover| track_hover_band(("usage-model-chart", "band"), hover, window, cx));
    }

    fn tooltip_state(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _cx: &App,
    ) -> Option<TooltipState> {
        let ModelLayout {
            plot_height,
            band_width,
            x,
            ..
        } = self.layout(bounds);
        if position.x < px(MODEL_AXIS_GUTTER) || position.y > px(plot_height) {
            return None;
        }
        let index = x.least_index(position.x.as_f32() - MODEL_AXIS_GUTTER);
        let center = x.tick(&self.days.get(index)?.label)? + band_width / 2.0 + MODEL_AXIS_GUTTER;
        Some(TooltipState::new(
            index,
            point(px(center), position.y),
            Vec::new(),
        ))
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<AnyElement> {
        let day = self.days.get(state.index)?;
        let ModelLayout {
            plot_height,
            band_width,
            ..
        } = self.layout(bounds);
        // The band follows the hover spring, as on the trend chart.
        let center = self.hover.map_or(state.cross_line, |hover| {
            point(hover.center, state.cross_line.y)
        });
        let mut tooltip = Tooltip::new(cursor, bounds.size)
            .gap(px(8.0))
            .cross_line(
                CrossLine::new(center)
                    .span(0.0, plot_height)
                    .band(px(band_width)),
            )
            // The basis the shares are computed from belongs in the title: the
            // rows below carry model shares, not raw values.
            .title(format!("{} · {}", day.label, self.metric_label));
        for ((category, value), share) in self
            .categories
            .iter()
            .zip(day.values.iter())
            .zip(day.shares.iter())
        {
            let value = value.map_or_else(
                || locale::text("Unknown", "未知", "未知").to_string(),
                |value| format!("{} · {:.1}%", format_full_number(value), share * 100.0),
            );
            tooltip = tooltip.row(category.color, category.label.clone(), value);
        }
        Some(tooltip.into_any_element())
    }
}

/// Stack `series_count` value slots per datum into one [`StackSeries`] per slot.
///
/// The keys are the slot indices; only their order matters, because the caller
/// zips the result back onto its own series list.
fn stack_series<T: Clone>(
    data: &[T],
    series_count: usize,
    value: impl Fn(&T, usize) -> Option<f32> + 'static,
) -> Vec<StackSeries<T>> {
    if data.is_empty() || series_count == 0 {
        return Vec::new();
    }
    Stack::new()
        .data(data.to_vec())
        .keys((0..series_count).map(|index| index.to_string()))
        .value(move |datum, key| {
            key.parse::<usize>()
                .ok()
                .and_then(|index| value(datum, index))
        })
        .series()
}

/// Normalize one day's category values to shares of that day's reported total.
///
/// Unknown categories keep a zero share (their segments are simply absent) and
/// the last reported share absorbs the rounding drift, so every column reaches
/// exactly 100%.
fn normalized_shares(values: &[Option<u64>]) -> Vec<f32> {
    let mut shares = vec![0.0; values.len()];
    let total = values
        .iter()
        .flatten()
        .fold(0_u64, |total, value| total.saturating_add(*value));
    if total == 0 {
        return shares;
    }
    let last_reported = values
        .iter()
        .rposition(|value| value.is_some_and(|value| value > 0));
    let mut assigned = 0.0_f32;
    for (index, value) in values.iter().enumerate() {
        let Some(value) = value else {
            continue;
        };
        if *value == 0 {
            continue;
        }
        let share = if Some(index) == last_reported {
            (1.0 - assigned).max(0.0)
        } else {
            *value as f32 / total as f32
        };
        shares[index] = share;
        assigned += share;
    }
    shares
}

/// Round a total up to the next readable Token axis maximum, at least
/// `minimum`, so the axis lands on whole K values.
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

/// The five evenly spaced Token axis values, from the maximum down to zero.
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

/// Format one Token axis tick in K units without trailing zeroes.
fn format_token_axis_k(value: u64) -> String {
    if value.is_multiple_of(1_000) {
        format!("{}K", value / 1_000)
    } else {
        let formatted = format!("{:.2}", value as f64 / 1_000.0);
        format!("{}K", formatted.trim_end_matches('0').trim_end_matches('.'))
    }
}

fn format_percentage_axis(share: f64) -> String {
    format!("{}%", (share * 100.0).round() as i64)
}

/// Keep at most this many band labels before thinning starts.
const TREND_LABEL_LIMIT: usize = 8;
/// Spacing between the thinned band labels of a dense range.
const TREND_LABEL_STEP: usize = 6;

/// The label interval a bucket count needs: every label while they fit, every
/// [`TREND_LABEL_STEP`]th otherwise.
fn trend_label_step(bucket_count: usize) -> usize {
    if bucket_count <= TREND_LABEL_LIMIT {
        1
    } else {
        TREND_LABEL_STEP
    }
}

/// Whether the bucket at `index` shows a label: dense ranges keep the first
/// and last bucket named and thin the rest.
fn trend_label_visible(index: usize, bucket_count: usize, step: usize) -> bool {
    bucket_count <= TREND_LABEL_LIMIT
        || index == 0
        || index + 1 == bucket_count
        || index.is_multiple_of(step)
}

/// Exact-value text shared by the chart tooltips: a missing metric stays
/// unknown instead of collapsing to zero.
fn metric_value_text(value: Option<u64>) -> String {
    value
        .map(format_full_number)
        .unwrap_or_else(|| locale::text("Unknown", "未知", "未知").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn trend_labels_thin_dense_ranges_without_losing_the_ends() {
        assert_eq!(trend_label_step(8), 1);
        assert_eq!(trend_label_step(30), TREND_LABEL_STEP);
        assert!(trend_label_visible(0, 30, TREND_LABEL_STEP));
        assert!(trend_label_visible(29, 30, TREND_LABEL_STEP));
        assert!(trend_label_visible(6, 30, TREND_LABEL_STEP));
        assert!(!trend_label_visible(7, 30, TREND_LABEL_STEP));
        assert!(trend_label_visible(7, 8, 1));
    }

    #[test]
    fn stacked_series_tile_every_bucket_and_keep_unknowns_out_of_the_stack() {
        let buckets = vec![
            TrendChartBucket::new("a", vec![Some(10), Some(5)]),
            TrendChartBucket::new("b", vec![None, Some(4)]),
        ];
        let stacked = stack_series(&buckets, 2, |bucket, index| {
            bucket.values[index].map(|value| value as f32)
        });

        assert_eq!(stacked.len(), 2);
        assert_eq!(stacked[0].points[0].y0, 0.0);
        assert_eq!(stacked[0].points[0].y1, 10.0);
        assert_eq!(stacked[1].points[0].y0, 10.0);
        assert_eq!(stacked[1].points[0].y1, 15.0);
        // An unknown metric contributes no height instead of a fabricated zero.
        assert_eq!(stacked[0].points[1].y1, 0.0);
        assert_eq!(stacked[1].points[1].y0, 0.0);
        assert_eq!(stacked[1].points[1].y1, 4.0);
    }

    #[test]
    fn model_shares_normalize_each_day_and_keep_unknowns_unknown() {
        assert_eq!(normalized_shares(&[Some(3), Some(1)]), [0.75, 0.25]);
        assert_eq!(normalized_shares(&[Some(0), Some(0)]), [0.0, 0.0]);
        assert_eq!(normalized_shares(&[None, Some(0)]), [0.0, 0.0]);
        assert_eq!(normalized_shares(&[None, Some(2), None]), [0.0, 1.0, 0.0]);

        // The reported shares always add up to a full column, including the
        // repeating fractions that cannot be represented exactly.
        let shares = normalized_shares(&[Some(1), Some(1), Some(1)]);
        assert_eq!(shares.iter().sum::<f32>(), 1.0);
    }
}

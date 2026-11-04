//! The Routing tab's request records, drawn as a `DataTable`.
//!
//! The rows and every value they state come from `routes.rs`. This module only
//! lays them out: one column per thing a reader compares — when, which Agent
//! and model, who answered, and what it cost.
//!
//! The delegate holds a snapshot instead of reading back into
//! [`ManagementCenter`]: the table renders and refreshes from inside that
//! view's own update, and a lease read from there panics with "cannot read
//! `ManagementCenter` while it is already being updated".

use super::routes::{
    RouteListRow, RouteRequestsData, RouteRequestsEmpty, UNKNOWN_AGENT, agent_hue, asked_model,
    cache_share, compact_tokens, gateway_clock, gateway_duration, outcome_answered, output_speed,
    said, wire_pair,
};
use super::*;

use gpui::{Div, Pixels, Stateful};
use std::rc::Rc;

/// The clock column's width. Its text never changes shape, so it does not
/// take part in the share-out; only the three text lanes grow.
const ROUTE_TIME_WIDTH: Pixels = px(96.0);

/// How much of the rest each text lane asks for. Asked takes the most: it
/// carries the Agent, the model and the route group's tag on one line.
const ROUTE_ASKED_SHARE: f32 = 0.38;
const ROUTE_ANSWERED_SHARE: f32 = 0.32;
const ROUTE_COST_SHARE: f32 = 0.30;

/// The shortest a lane may be dragged. Below this even the heading stops
/// being readable, so the drag stops there.
const ROUTE_LANE_FLOOR: Pixels = px(72.0);

/// The empty column the table paints after the last lane, which takes width
/// from the same row. Subtracting it keeps the lanes from overflowing the
/// table by a few pixels and raising a scrollbar nobody asked for.
const ROUTE_TRAILING_WIDTH: Pixels = px(12.0);

/// How the four lanes fill the table.
///
/// The clock keeps a fixed width and the three text lanes share the rest, so
/// the records fill the card instead of hugging its left edge and leaving the
/// right half empty. The shares always sum to the room there is, so the lanes
/// do not overflow and raise a scrollbar nobody asked for. A table too narrow
/// for three readable lanes keeps them at the floor and scrolls instead of
/// squeezing the headings into one another.
fn route_lane_widths(available: Pixels) -> [Pixels; 4] {
    let usable = (f32::from(available) - f32::from(ROUTE_TRAILING_WIDTH)).max(0.0);
    let rest = (usable - f32::from(ROUTE_TIME_WIDTH)).max(0.0);
    let lanes = [
        (rest * ROUTE_ASKED_SHARE).max(f32::from(ROUTE_LANE_FLOOR)),
        (rest * ROUTE_ANSWERED_SHARE).max(f32::from(ROUTE_LANE_FLOOR)),
        (rest * ROUTE_COST_SHARE).max(f32::from(ROUTE_LANE_FLOOR)),
    ];
    [ROUTE_TIME_WIDTH, px(lanes[0]), px(lanes[1]), px(lanes[2])]
}

/// Whether two layouts place every lane at the same pixel.
fn same_widths(left: [Pixels; 4], right: [Pixels; 4]) -> bool {
    left.iter()
        .zip(&right)
        .all(|(left, right)| (f32::from(*left) - f32::from(*right)).abs() <= 0.5)
}

/// The records table, kept in step with the rows it paints.
pub(in crate::management) struct RouteRequestsTableDelegate {
    /// The Routing tab, which picks a request when its record is clicked.
    view: WeakEntity<ManagementCenter>,
    /// The rows, and everything the rows say besides the record itself.
    data: Rc<RouteRequestsSnapshot>,
    /// The widths the table is told to draw with.
    widths: [Pixels; 4],
    /// The width the current layout was chosen for. A drag keeps the widths
    /// it produced while this still matches; a window that changes size lays
    /// the lanes out afresh.
    laid_for: Pixels,
}

/// What the records table paints, as one value.
pub(super) struct RouteRequestsSnapshot {
    pub rows: Vec<RouteListRow>,
    /// The request the map is pinned to, so its record shows as picked.
    pub pinned: Option<vibex_core::RequestId>,
    /// When the rows were read, for a live request's elapsed time.
    pub now: i64,
    /// What those rows say besides the record itself.
    pub data: Rc<dyn RouteRequestsData>,
    /// Why a table with no rows has none.
    pub empty: RouteRequestsEmpty,
    /// The first page is still on its way.
    pub loading: bool,
}

impl RouteRequestsTableDelegate {
    pub(super) fn new(view: WeakEntity<ManagementCenter>, data: Rc<RouteRequestsSnapshot>) -> Self {
        Self {
            view,
            data,
            widths: route_lane_widths(px(0.0)),
            laid_for: px(0.0),
        }
    }

    /// Takes the width the table has to fill, as the tab measured it.
    ///
    /// Returns whether the columns moved: the table has to be refreshed then,
    /// because it keeps its own copy of the widths once laid out. A drag only
    /// lasts as long as the table keeps its width; a resize lays the lanes out
    /// afresh, since a drag's widths were chosen against the old row.
    pub(super) fn set_available(&mut self, available: Pixels) -> bool {
        if !f32::from(available).is_finite() || available <= px(0.0) {
            return false;
        }
        if (f32::from(available) - f32::from(self.laid_for)).abs() <= 0.5 {
            return false;
        }
        self.laid_for = available;
        let widths = route_lane_widths(available);
        let changed = !same_widths(widths, self.widths);
        self.widths = widths;
        changed
    }

    /// Folds in the layout the reader dragged out.
    ///
    /// Returns whether the columns moved. The drag outranks the share-out at
    /// the width it was made at, so the lanes stay where they were put.
    pub(super) fn set_chosen(&mut self, chosen: [Pixels; 4]) -> bool {
        let changed = !same_widths(chosen, self.widths);
        self.widths = chosen;
        changed
    }

    /// Takes the rows and the filters as they are now, and says whether the
    /// table has anything new to show.
    pub(super) fn set_snapshot(&mut self, data: Rc<RouteRequestsSnapshot>) -> bool {
        let rows_changed = self.data.rows.len() != data.rows.len()
            || self
                .data
                .rows
                .iter()
                .zip(&data.rows)
                .any(|(before, now)| before.id() != now.id());
        let empty_changed = self.data.empty != data.empty || self.data.loading != data.loading;
        let picked_changed = self.data.pinned != data.pinned;
        // A live record states how long it has waited, so it is redrawn as
        // that clock moves even when the record itself has not changed.
        let clock_moved = data.now / 1_000 != self.data.now / 1_000;
        self.data = data;
        rows_changed || empty_changed || picked_changed || clock_moved
    }

    fn row(&self, row_ix: usize) -> Option<&RouteListRow> {
        self.data.rows.get(row_ix)
    }
}

/// The records' columns, in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RouteColumn {
    Time,
    Asked,
    Answered,
    Cost,
}

impl RouteColumn {
    const ALL: [Self; 4] = [Self::Time, Self::Asked, Self::Answered, Self::Cost];

    fn name(self) -> &'static str {
        match self {
            Self::Time => management_locale_text("Time", "时间", "時間"),
            Self::Asked => management_locale_text(
                "Agent · model asked",
                "Agent · 请求模型",
                "Agent · 請求模型",
            ),
            Self::Answered => management_locale_text("Answered by", "响应方", "回應方"),
            Self::Cost => management_locale_text(
                "Time · tokens · cache · speed",
                "用时 · Token · 缓存 · 速度",
                "用時 · Token · 快取 · 速度",
            ),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Time => "time",
            Self::Asked => "asked",
            Self::Answered => "answered",
            Self::Cost => "cost",
        }
    }

    /// Which column a record's field lands in. The head and the cells share
    /// this map, so a cell can never drift away from its heading.
    fn at(col_ix: usize) -> Self {
        Self::ALL.get(col_ix).copied().unwrap_or(Self::Time)
    }

    fn column(self, width: Pixels) -> Column {
        // The widget's own cell padding is all these lanes need.
        let column = Column::new(self.key(), self.name())
            .width(width)
            .min_width(ROUTE_LANE_FLOOR)
            .resizable(true);
        match self {
            Self::Cost => column.text_right(),
            Self::Time | Self::Asked | Self::Answered => column,
        }
    }
}

impl TableDelegate for RouteRequestsTableDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        RouteColumn::ALL.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.data.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let lane = col_ix.min(RouteColumn::ALL.len() - 1);
        RouteColumn::at(col_ix).column(self.widths[lane])
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let name = self.column(col_ix, cx).name;
        let right = RouteColumn::at(col_ix) == RouteColumn::Cost;
        div()
            .size_full()
            .flex()
            .items_center()
            .when(right, |cell| cell.justify_end())
            .truncate()
            .child(name)
            .into_any_element()
    }

    /// The gutter after the last lane.
    ///
    /// The kit's own is a rem-sized box, which would leave the share-out a
    /// fraction of a lane short or long whenever the root font differs from
    /// the default. This one is the constant the layout subtracts, so the four
    /// lanes and the gutter always add up to the table exactly.
    fn render_last_empty_col(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div().w(ROUTE_TRAILING_WIDTH).h_full().flex_shrink_0()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(row) = self.row(row_ix) else {
            return div().into_any_element();
        };
        let parts = route_row_parts(row, &self.data, cx);
        match RouteColumn::at(col_ix) {
            RouteColumn::Time => div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .font_family(cx.theme().mono_font_family.clone())
                .child(gateway_clock(parts.at_ms))
                .into_any_element(),
            RouteColumn::Asked => h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap_1p5()
                .child(div().flex_none().size(px(8.0)).rounded_full().bg(parts.hue))
                .child(agent_brand_icon(&parts.agent_key, px(14.0), None))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(parts.asked),
                )
                .when_some(parts.route, |line, route| {
                    line.child(
                        div()
                            .flex_none()
                            .max_w(rems(8.0))
                            .truncate()
                            .px_1p5()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .text_size(px(10.5))
                            .text_color(cx.theme().muted_foreground)
                            .child(route),
                    )
                })
                .into_any_element(),
            RouteColumn::Answered => {
                let id = row.id().clone();
                let click_id = id.clone();
                let debug_id = id.clone();
                let failed = parts.failed;
                // The record is the way into its route: a click picks it and
                // pins the map above, a second click replays it.
                let on_click = {
                    let view = self.view.clone();
                    cx.listener(move |_, _, _, cx| {
                        let _ = view
                            .update(cx, |view, cx| view.pick_gateway_route(click_id.clone(), cx));
                    })
                };
                h_flex()
                    .id(SharedString::from(format!(
                        "management-gateway-request-row-{}",
                        id.as_str()
                    )))
                    .debug_selector(move || {
                        format!("management-gateway-request-row-{}", debug_id.as_str())
                    })
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap_1p5()
                    .on_click(on_click)
                    .child(div().flex_none().size(px(6.0)).rounded_full().bg(parts.dot))
                    .child(
                        div()
                            .flex_none()
                            .max_w(rems(10.0))
                            .truncate()
                            .text_xs()
                            .font_medium()
                            .text_color(parts.who_color)
                            .child(parts.who),
                    )
                    .when(!parts.detail.is_empty(), |line| {
                        line.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(if failed {
                                    cx.theme().danger
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(parts.detail),
                        )
                    })
                    .children(
                        parts
                            .tags
                            .into_iter()
                            .map(|tag| div().flex_none().child(tag)),
                    )
                    .into_any_element()
            }
            RouteColumn::Cost => div()
                .w_full()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .font_family(cx.theme().mono_font_family.clone())
                .child(parts.metrics.join(" · "))
                .into_any_element(),
        }
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let text = if self.data.loading {
            management_locale_text(
                "Loading request records…",
                "正在加载请求记录…",
                "正在載入請求記錄…",
            )
        } else {
            self.data.empty.line()
        };
        h_flex()
            .size_full()
            .justify_center()
            .px_4()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let row = div().id(("management-gateway-request", row_ix));
        let Some(record) = self.row(row_ix) else {
            return row;
        };
        // A picked record keeps the picked look it had in the list, so the
        // map above and the record below always name the same request. The
        // table merges this row's style after its own, so the plate and the
        // left edge win over the row background it would otherwise draw.
        if self.data.pinned.as_ref() == Some(record.id()) {
            row.bg(cx.theme().list_active)
                .border_l_2()
                .border_color(cx.theme().list_active_border)
        } else {
            row
        }
    }
}

/// What one record says, cell by cell.
struct RouteRowParts {
    at_ms: i64,
    agent_key: String,
    asked: String,
    hue: Hsla,
    route: Option<String>,
    failed: bool,
    /// The state dot's colour, and the colour the answering side is said in.
    dot: Hsla,
    who_color: Hsla,
    who: String,
    detail: String,
    tags: Vec<AnyElement>,
    metrics: Vec<String>,
}

fn route_row_parts(
    row: &RouteListRow,
    snapshot: &RouteRequestsSnapshot,
    cx: &App,
) -> RouteRowParts {
    let locale = locale::current_locale();
    let (at_ms, agent, requested) = match row {
        RouteListRow::Live(live) => (
            live.started_at_ms,
            live.agent_id.clone(),
            live.requested_model.clone(),
        ),
        RouteListRow::Done(done) => (
            done.created_at_ms,
            done.agent_id.clone(),
            done.model_requested.clone(),
        ),
    };
    let agent_key = agent.unwrap_or_else(|| UNKNOWN_AGENT.to_string());
    let route = snapshot.data.route_name(&requested);
    let failed = matches!(row, RouteListRow::Done(done) if !outcome_answered(done.outcome));
    let mut tags = Vec::<AnyElement>::new();
    let (dot, who, detail, metrics) = match row {
        RouteListRow::Live(live) => {
            let waited = (snapshot.now - live.started_at_ms).max(0);
            if live.attempt > 1 {
                tags.push(
                    Tag::color(ColorName::Blue)
                        .xsmall()
                        .child(said(
                            locale,
                            format!("member {}", live.attempt),
                            format!("第 {} 个成员", live.attempt),
                            format!("第 {} 個成員", live.attempt),
                        ))
                        .into_any_element(),
                );
            }
            let mut metrics = vec![gateway_duration(waited)];
            if let Some(first) = live.first_token_at_ms {
                let first = gateway_duration((first - live.started_at_ms).max(0));
                metrics.push(said(
                    locale,
                    format!("first token {first}"),
                    format!("首 Token {first}"),
                    format!("首 Token {first}"),
                ));
            }
            let state = if live.first_token_at_ms.is_some() {
                management_locale_text("streaming", "正在输出", "正在輸出")
            } else {
                management_locale_text("waiting", "等待响应", "等待回應")
            };
            (
                (cx.theme().info, cx.theme().foreground),
                snapshot.data.provider_name(&live.model_provider_profile_id),
                state.to_string(),
                metrics,
            )
        }
        RouteListRow::Done(done) => {
            let walk = snapshot.data.ledger_walk(done);
            let moves = snapshot.data.walk_moves(&walk);
            let answered = outcome_answered(done.outcome);
            let dot = if !answered {
                (cx.theme().danger, cx.theme().danger)
            } else if moves > 0 {
                (cx.theme().primary, cx.theme().foreground)
            } else {
                (cx.theme().success, cx.theme().foreground)
            };
            if answered && moves > 0 {
                tags.push(
                    Tag::color(ColorName::Blue)
                        .xsmall()
                        .child(said(
                            locale,
                            format!("rerouted ×{moves}"),
                            format!("转移 ×{moves}"),
                            format!("轉移 ×{moves}"),
                        ))
                        .into_any_element(),
                );
            }
            let who = if done.outcome == vibex_core::GatewayRequestOutcome::Rejected {
                management_locale_text("gateway", "网关", "閘道").to_string()
            } else {
                snapshot.data.provider_label(
                    &done.model_provider_profile_id,
                    Some(done.provider_name.as_str()),
                )
            };
            let detail = if answered {
                let key = gateway_credential_name(done.credential_name.as_deref(), locale);
                match done.model_served.as_deref() {
                    Some(served) if served != asked_model(&done.model_requested) => {
                        format!("{key} · {served}")
                    }
                    _ => key.to_string(),
                }
            } else {
                snapshot.data.failure_text(done, &walk)
            };
            let mut metrics = Vec::new();
            if let Some(latency) = done.latency_ms {
                metrics.push(gateway_duration(latency));
            }
            if done.total_tokens > 0 {
                metrics.push(format!("{} tok", compact_tokens(done.total_tokens)));
            }
            if let Some(share) = cache_share(done) {
                metrics.push(said(
                    locale,
                    format!("{share}% cached"),
                    format!("缓存 {share}%"),
                    format!("快取 {share}%"),
                ));
            }
            if let Some(speed) = output_speed(done) {
                metrics.push(format!("{speed} tok/s"));
            }
            (dot, who, detail, metrics)
        }
    };
    let protocol = match row {
        RouteListRow::Live(live) => wire_pair(&live.wire_in, &live.wire_protocol_id),
        RouteListRow::Done(done) => wire_pair(&done.wire_in, &done.wire_out),
    };
    // The protocols lead the row's tags: which wire came in, which went out.
    tags.insert(
        0,
        div()
            .flex_none()
            .max_w(rems(11.0))
            .truncate()
            .px_1p5()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .text_size(px(10.5))
            .font_family(cx.theme().mono_font_family.clone())
            .text_color(cx.theme().muted_foreground)
            .child(protocol)
            .into_any_element(),
    );
    RouteRowParts {
        at_ms,
        hue: agent_hue(&agent_key, cx),
        agent_key,
        asked: asked_model(&requested).to_string(),
        route,
        failed,
        dot: dot.0,
        who_color: dot.1,
        who,
        detail,
        tags,
        metrics,
    }
}

#[cfg(test)]
mod tests {
    use super::super::routes::WalkStep;
    use super::*;

    fn widths(available: f32) -> [f32; 4] {
        route_lane_widths(px(available)).map(f32::from)
    }

    /// The lanes fill the row they are given: no leftover on the right, and
    /// no overflow that would raise a horizontal scrollbar for nothing.
    #[test]
    fn the_lanes_share_the_width_they_are_given() {
        for available in [640.0f32, 900.0, 1_280.0, 1_880.0] {
            let lanes = widths(available);
            let total: f32 = lanes.iter().sum();
            assert!(
                total >= available - f32::from(ROUTE_TRAILING_WIDTH) - 0.5
                    && total <= available - f32::from(ROUTE_TRAILING_WIDTH) + 0.5,
                "{available}px should be shared out, got {total}px from {lanes:?}"
            );
            assert!(lanes.iter().all(|lane| *lane > 0.0));
        }
    }

    /// The clock keeps its width and the text lanes grow with the window, so a
    /// wider card reads more of each record rather than leaving air on the
    /// right.
    #[test]
    fn a_wider_table_gives_the_text_lanes_more_room() {
        let narrow = widths(700.0);
        let wide = widths(1_400.0);
        assert_eq!(narrow[0], wide[0], "the clock column is fixed");
        for lane in 1..4 {
            assert!(
                wide[lane] > narrow[lane],
                "lane {lane} should grow: {} -> {}",
                narrow[lane],
                wide[lane]
            );
        }
    }

    /// A very narrow window keeps the lanes readable instead of crushing
    /// them; the table scrolls sideways from there.
    #[test]
    fn a_narrow_table_keeps_the_lanes_readable() {
        let lanes = route_lane_widths(px(200.0));
        assert_eq!(lanes[1], ROUTE_LANE_FLOOR);
        assert_eq!(lanes[2], ROUTE_LANE_FLOOR);
        assert_eq!(lanes[3], ROUTE_LANE_FLOOR);
        assert!(
            lanes.iter().map(|lane| f32::from(*lane)).sum::<f32>() > 200.0,
            "a table this narrow scrolls rather than cutting the headings"
        );
    }

    /// Dragging a boundary outranks the share-out at the width it was made
    /// at, and resizing the window lays the lanes out again.
    #[test]
    fn a_dragged_layout_wins_until_the_table_resizes() {
        let mut delegate = RouteRequestsTableDelegate {
            view: WeakEntity::new_invalid(),
            data: Rc::new(RouteRequestsSnapshot {
                rows: Vec::new(),
                pinned: None,
                now: 0,
                data: Rc::new(EmptyFacts),
                empty: RouteRequestsEmpty::Range,
                loading: false,
            }),
            widths: route_lane_widths(px(1000.0)),
            laid_for: px(1000.0),
        };
        let dragged = [px(96.0), px(520.0), px(220.0), px(164.0)];
        assert!(delegate.set_chosen(dragged));
        // The widths are read straight off the delegate: `column` needs an
        // `App`, and the layout is what this test is about.
        assert_eq!(delegate.widths[1], dragged[1]);
        // The same width arriving again leaves the drag alone.
        assert!(!delegate.set_available(px(1000.0)));
        assert_eq!(delegate.widths, dragged);
        // A new width is a new row, so the lanes are laid out for it.
        assert!(delegate.set_available(px(1300.0)));
        assert_ne!(delegate.widths, dragged);
        assert_eq!(delegate.widths[0], ROUTE_TIME_WIDTH);
    }

    struct EmptyFacts;

    impl RouteRequestsData for EmptyFacts {
        fn route_name(&self, _model: &str) -> Option<String> {
            None
        }
        fn provider_name(&self, _profile: &vibex_core::ModelProviderProfileId) -> String {
            String::new()
        }
        fn provider_label(
            &self,
            _profile: &vibex_core::ModelProviderProfileId,
            _recorded: Option<&str>,
        ) -> String {
            String::new()
        }
        fn ledger_walk(&self, _request: &vibex_core::GatewayRequestSummary) -> Vec<WalkStep> {
            Vec::new()
        }
        fn walk_moves(&self, _walk: &[WalkStep]) -> usize {
            0
        }
        fn failure_text(
            &self,
            _request: &vibex_core::GatewayRequestSummary,
            _walk: &[WalkStep],
        ) -> String {
            String::new()
        }
    }
}

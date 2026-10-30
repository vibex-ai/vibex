//! The configuration centre's Provider workspace.
//!
//! One page owns three kinds of record — the shared Providers, the route
//! groups that compose them, and the gateway that serves the groups — so they
//! share one left column and one detail frame. Every detail view is drawn the
//! same way: an identity header, underline tabs, then section cards. The
//! records, their mutations and their drafts stay on [`ManagementCenter`];
//! this module only decides how they read.

use super::*;

mod create;
mod gateway;
mod provider;
mod route_group;
mod route_stage;
mod routes;
mod sidebar;

pub(crate) use route_stage::RouteStage;

impl ManagementCenter {
    /// The detail pane: whichever record the left column selected, or the
    /// draft the page is authoring.
    pub(super) fn render_provider_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Authoring replaces the detail pane so the list the new record lands
        // in stays visible beside it.
        let page = if self.provider_create_page_open {
            self.render_workspace_provider_create(window, cx)
        } else if self.route_group_page_open {
            self.render_workspace_route_group_editor(window, cx)
        } else if self.selected_global_provider_id.as_deref() == Some(GATEWAY_ROW_ID) {
            self.render_workspace_gateway(window, cx)
        } else if self.providers_sidebar_tab == ProviderSidebarTab::RouteGroups {
            self.render_workspace_route_group(window, cx)
        } else {
            let selected = self.selected_global_provider_id.clone();
            match self
                .shared_providers()
                .into_iter()
                .find(|row| Some(row.profile.id.as_str()) == selected.as_deref())
            {
                Some(row) => self.render_workspace_provider(row, window, cx),
                // No selection, or a Provider that has gone away, resolves to
                // the gateway: it is the page's default subject.
                None => self.render_workspace_gateway(window, cx),
            }
        };
        div()
            .id("management-provider-workspace")
            .w_full()
            .min_w_0()
            .child(page)
            .into_any_element()
    }

    /// Selects one Provider in the left column.
    fn select_workspace_provider(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected_global_provider_id = Some(id);
        self.selected_global_route_id = None;
        self.providers_sidebar_tab = ProviderSidebarTab::Providers;
        self.provider_create_page_open = false;
        self.route_group_page_open = false;
        cx.notify();
    }

    /// Selects one route group in the left column.
    fn select_workspace_route_group(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected_global_route_id = Some(id);
        self.selected_global_provider_id = None;
        self.providers_sidebar_tab = ProviderSidebarTab::RouteGroups;
        self.provider_create_page_open = false;
        self.route_group_page_open = false;
        cx.notify();
    }

    /// Selects the gateway in the left column.
    fn select_workspace_gateway(&mut self, cx: &mut Context<Self>) {
        self.selected_global_provider_id = Some(GATEWAY_ROW_ID.to_string());
        self.selected_global_route_id = None;
        self.providers_sidebar_tab = ProviderSidebarTab::Providers;
        self.provider_create_page_open = false;
        self.route_group_page_open = false;
        cx.notify();
    }

    /// The Agents that may bind a model source, in the order the snapshot
    /// lists them.
    fn workspace_configurable_agents(&self) -> Vec<(String, String)> {
        let configurable = vibex_core::model_provider_configurable_agent_ids().unwrap_or_default();
        self.snapshot
            .agents
            .iter()
            .filter(|agent| vibex_core::is_user_visible_agent(&agent.id))
            .filter(|agent| configurable.contains(&agent.id))
            .map(|agent| (agent.id.as_str().to_string(), agent.label.clone()))
            .collect()
    }

    fn workspace_agent_label(&self, agent_id: &str) -> String {
        self.snapshot
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == agent_id)
            .map(|agent| agent.label.clone())
            .unwrap_or_else(|| agent_id.to_string())
    }

    /// The traffic range as a menu button, shared by every traffic view so a
    /// change on one page holds on the others.
    fn workspace_range_menu(&self, id: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let active = self.traffic_range;
        let center = cx.weak_entity();
        Button::new(id)
            .small()
            .outline()
            .icon(Icon::default().path("icons/vibex/clock.svg"))
            .label(active.label())
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for range in TrafficRange::ALL {
                    let center = center.clone();
                    menu = menu.item(
                        PopupMenuItem::new(range.label())
                            .checked(range == active)
                            .on_click(move |_, _, cx| {
                                center
                                    .update(cx, |this, cx| {
                                        this.traffic_range = range;
                                        cx.notify();
                                    })
                                    .ok();
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }
}

/// The figures every traffic view reports, computed once per view.
struct WorkspaceTraffic {
    total: usize,
    succeeded: usize,
    tokens: u64,
    p50: Option<i64>,
    p95: Option<i64>,
    top_error: Option<(u16, usize)>,
}

impl WorkspaceTraffic {
    fn collect<'a>(requests: impl Iterator<Item = &'a vibex_core::GatewayRequestSummary>) -> Self {
        let requests = requests.collect::<Vec<_>>();
        let succeeded = requests
            .iter()
            .filter(|request| request.status.is_some_and(|status| status < 400))
            .count();
        let tokens = requests.iter().map(|request| request.total_tokens).sum();
        let latencies = requests
            .iter()
            .filter_map(|request| request.latency_ms)
            .collect::<Vec<_>>();
        let mut errors = BTreeMap::<u16, usize>::new();
        for request in &requests {
            if let Some(status) = request.status.filter(|status| *status >= 400) {
                *errors.entry(status).or_default() += 1;
            }
        }
        Self {
            total: requests.len(),
            succeeded,
            tokens,
            p50: management_percentile(latencies.clone(), 0.5),
            p95: management_percentile(latencies, 0.95),
            top_error: errors.into_iter().max_by_key(|(_, count)| *count),
        }
    }

    fn success_caption(&self) -> String {
        if self.total == 0 {
            management_locale_text("No traffic yet", "还没有流量", "還沒有流量").to_string()
        } else {
            format!(
                "{} {:.0}%",
                management_locale_text("Success", "成功率", "成功率"),
                self.succeeded as f64 * 100.0 / self.total as f64
            )
        }
    }

    fn latency(value: Option<i64>) -> String {
        value
            .map(|value| format!("{value} ms"))
            .unwrap_or_else(|| "—".to_string())
    }

    fn top_error_label(&self) -> String {
        self.top_error
            .map(|(status, count)| format!("HTTP {status} × {count}"))
            .unwrap_or_else(|| {
                management_locale_text("No errors", "没有错误", "沒有錯誤").to_string()
            })
    }

    /// The four tiles a traffic view leads with.
    fn tiles(&self, cx: &App) -> AnyElement {
        ws_tile_row(vec![
            management_stat_tile(
                management_locale_text("Requests", "请求", "請求"),
                self.total.to_string(),
                self.success_caption(),
                None,
                cx,
            ),
            management_stat_tile(
                management_locale_text("Latency p50", "延迟 p50", "延遲 p50"),
                Self::latency(self.p50),
                format!("p95 {}", Self::latency(self.p95)),
                None,
                cx,
            ),
            management_stat_tile(
                "Token",
                gateway_token_count(self.tokens),
                management_locale_text("Counted by the gateway", "网关计数", "閘道計數")
                    .to_string(),
                None,
                cx,
            ),
            management_stat_tile(
                management_locale_text("Errors", "错误", "錯誤"),
                (self.total - self.succeeded).to_string(),
                self.top_error_label(),
                (self.total > self.succeeded).then(|| cx.theme().warning),
                cx,
            ),
        ])
    }
}

/// A row of stat tiles that wraps when the pane narrows.
fn ws_tile_row(tiles: Vec<AnyElement>) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .flex_wrap()
        .gap_3()
        .children(tiles)
        .into_any_element()
}

/// A square plate behind a monochrome icon: the leading mark of a record that
/// has no logo of its own (the gateway, a route group).
fn ws_icon_tile(path: &'static str, size: Pixels, tone: Option<Hsla>, cx: &App) -> AnyElement {
    let tone = tone.unwrap_or(cx.theme().muted_foreground);
    div()
        .flex_none()
        .size(size)
        .flex()
        .items_center()
        .justify_center()
        .rounded(cx.theme().radius_lg)
        .bg(tone.opacity(0.12))
        .child(Icon::default().path(path).size(size * 0.5).text_color(tone))
        .into_any_element()
}

/// The identity header every detail view opens with.
///
/// The mark and the title lead; one muted line of facts sits under the
/// title; the commands that act on the whole record hold the trailing edge.
fn ws_page_header(
    mark: AnyElement,
    title: AnyElement,
    facts: AnyElement,
    actions: AnyElement,
) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap_3()
        .child(mark)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(title)
                .child(facts),
        )
        .child(h_flex().flex_none().items_center().gap_2().child(actions))
        .into_any_element()
}

/// A title with the badges that qualify it.
fn ws_title(text: impl Into<SharedString>, badges: Vec<AnyElement>) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap_2()
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_xl()
                .font_semibold()
                .child(text.into()),
        )
        .children(
            badges
                .into_iter()
                .map(|badge| div().flex_none().child(badge)),
        )
        .into_any_element()
}

/// One muted line of facts separated by middle dots.
fn ws_facts(items: Vec<AnyElement>, cx: &App) -> AnyElement {
    let mut line = h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .flex_wrap()
        .gap_x_2()
        .gap_y_0p5()
        .text_xs()
        .text_color(cx.theme().muted_foreground);
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            line = line.child(div().flex_none().child("·"));
        }
        line = line.child(item);
    }
    line.into_any_element()
}

/// A fact drawn in the monospace face: an address, an id.
fn ws_mono_fact(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .min_w_0()
        .max_w(rems(26.0))
        .truncate()
        .font_family(cx.theme().mono_font_family.clone())
        .child(text.into())
        .into_any_element()
}

/// A coloured dot beside the word it stands for; colour never carries the
/// state alone.
fn ws_state_fact(label: impl Into<SharedString>, color: Hsla) -> AnyElement {
    h_flex()
        .flex_none()
        .items_center()
        .gap_1()
        .child(div().size(px(6.0)).rounded_full().bg(color))
        .child(div().text_color(color).child(label.into()))
        .into_any_element()
}

/// Underline tabs under a detail header.
fn ws_tabs(
    id: &'static str,
    labels: Vec<&'static str>,
    selected: usize,
    on_click: impl Fn(&mut ManagementCenter, usize, &mut Window, &mut Context<ManagementCenter>)
    + 'static,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    TabBar::new(id)
        .w_full()
        .underline()
        .selected_index(selected)
        .children(labels.into_iter().map(|label| {
            Tab::new()
                .aria_label(label)
                .child(div().text_sm().child(label))
        }))
        .on_click(
            cx.listener(move |this, index: &usize, window, cx| on_click(this, *index, window, cx)),
        )
        .into_any_element()
}

/// A section card: a heading band with an optional trailing action, then the
/// content. Sections stack with one gap; cards never nest.
fn ws_section(
    title: &'static str,
    description: Option<&'static str>,
    action: Option<AnyElement>,
    content: AnyElement,
    cx: &App,
) -> AnyElement {
    v_flex()
        .w_full()
        .min_w_0()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(cx.theme().border)
        .bg(theme::semantic_color("card", cx.theme().is_dark()).opacity(0.72))
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap_3()
                .px_4()
                .pt_3()
                .pb_2()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().text_sm().font_semibold().child(title))
                        .children(description.map(|description| {
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(description)
                        })),
                )
                .children(action.map(|action| div().flex_none().child(action))),
        )
        .child(div().w_full().min_w_0().px_4().pb_3().child(content))
        .into_any_element()
}

/// A page's stack of sections.
fn ws_stack(children: Vec<AnyElement>) -> AnyElement {
    v_flex()
        .w_full()
        .min_w_0()
        .gap_4()
        .children(children)
        .into_any_element()
}

/// A labelled form control with optional help underneath.
fn ws_field(
    label: &'static str,
    hint: Option<&'static str>,
    control: AnyElement,
    cx: &App,
) -> AnyElement {
    v_flex()
        .w_full()
        .min_w_0()
        .gap_1p5()
        .child(div().text_sm().font_medium().child(label))
        .child(control)
        .children(hint.map(|hint| {
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(hint)
        }))
        .into_any_element()
}

/// One text input bound to a draft, optionally masked and flagged.
fn ws_input(state: &Entity<InputState>, masked: bool, error: Option<&str>, cx: &App) -> AnyElement {
    let mut input = Input::new(state).w_full();
    if masked {
        input = input.mask_toggle();
    }
    if error.is_some() {
        input = input.border_color(cx.theme().danger);
    }
    v_flex()
        .w_full()
        .min_w_0()
        .gap_1()
        .child(input)
        .children(error.map(|message| {
            div()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(message.to_string())
        }))
        .into_any_element()
}

/// Two fields side by side, wrapping under each other when narrow.
fn ws_field_pair(left: AnyElement, right: AnyElement) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .items_start()
        .flex_wrap()
        .gap_4()
        .child(div().flex_1().min_w(rems(14.0)).child(left))
        .child(div().flex_1().min_w(rems(14.0)).child(right))
        .into_any_element()
}

/// A mutually exclusive choice drawn as a row of selectable cards: each says
/// what it is and what it does, so the decision is made before the click.
fn ws_choice_cards(
    id: &'static str,
    choices: Vec<(&'static str, SharedString, Option<SharedString>)>,
    selected: Option<&str>,
    disabled: bool,
    on_pick: impl Fn(&mut ManagementCenter, &'static str, &mut Context<ManagementCenter>) + 'static,
    cx: &mut Context<ManagementCenter>,
) -> AnyElement {
    let on_pick = Rc::new(on_pick);
    h_flex()
        .w_full()
        .min_w_0()
        .flex_wrap()
        .gap_2()
        .children(choices.into_iter().map(|(value, title, description)| {
            let active = selected == Some(value);
            let on_pick = on_pick.clone();
            v_flex()
                .id(SharedString::from(format!("{id}-{value}")))
                .flex_1()
                .min_w(rems(9.0))
                .gap_0p5()
                .px_3()
                .py_2()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(if active {
                    cx.theme().primary
                } else {
                    cx.theme().border
                })
                .bg(if active {
                    cx.theme().primary.opacity(0.08)
                } else {
                    cx.theme().background.opacity(0.0)
                })
                .when(!disabled, |card| {
                    card.cursor_pointer()
                        .hover(|style| style.bg(cx.theme().primary.opacity(0.05)))
                        .on_click(cx.listener(move |this, _, _, cx| on_pick(this, value, cx)))
                })
                .when(disabled, |card| card.opacity(0.6))
                .child(
                    h_flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .flex_none()
                                .size(px(12.0))
                                .rounded_full()
                                .border_1()
                                .border_color(if active {
                                    cx.theme().primary
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .when(active, |dot| {
                                    dot.child(
                                        div()
                                            .m(px(2.0))
                                            .size(px(6.0))
                                            .rounded_full()
                                            .bg(cx.theme().primary),
                                    )
                                }),
                        )
                        .child(div().text_sm().font_medium().child(title)),
                )
                .children(description.map(|description| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description)
                }))
        }))
        .into_any_element()
}

/// A list row with a leading mark, a title and subtitle, and trailing
/// controls; rows inside one section are separated by hairlines.
fn ws_row(
    id: SharedString,
    mark: AnyElement,
    title: AnyElement,
    subtitle: Option<AnyElement>,
    trailing: AnyElement,
    first: bool,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    h_flex()
        .id(id)
        .w_full()
        .min_w_0()
        .items_center()
        .gap_3()
        .py_2p5()
        .when(!first, |row| {
            row.border_t_1()
                .border_color(cx.theme().border.opacity(0.6))
        })
        .child(mark)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(title)
                .children(subtitle),
        )
        .child(h_flex().flex_none().items_center().gap_2().child(trailing))
}

/// The muted empty line a section shows when it has nothing to list.
fn ws_empty_line(text: &'static str, cx: &App) -> AnyElement {
    div()
        .w_full()
        .py_3()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// The bottom band of a form: the error, then Cancel and the commit.
fn ws_form_footer(error: Option<String>, actions: Vec<AnyElement>, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .gap_2()
        .pt_4()
        .border_t_1()
        .border_color(cx.theme().border)
        .children(error.map(|message| {
            div()
                .w_full()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(message)
        }))
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_end()
                .gap_2()
                .children(actions),
        )
        .into_any_element()
}

/// A page whose subject is a draft: a Back command, the title and its
/// one-line purpose, then the form.
fn ws_authoring_page(
    id: &'static str,
    back: AnyElement,
    mark: Option<AnyElement>,
    title: impl Into<SharedString>,
    description: &'static str,
    body: AnyElement,
    cx: &App,
) -> AnyElement {
    v_flex()
        .id(id)
        .w_full()
        .min_w_0()
        .gap_5()
        .child(div().child(back))
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap_3()
                .children(mark)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_xl().font_semibold().child(title.into()))
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(description),
                        ),
                ),
        )
        .child(body)
        .into_any_element()
}

/// The Agents that use a model source and the ones that could, each with a
/// switch that says the state and changes it.
struct WorkspaceAgentRow {
    agent_id: String,
    label: String,
    bound: bool,
    is_default: bool,
}

fn ws_agent_mark(agent_id: &str, cx: &App) -> AnyElement {
    div()
        .flex_none()
        .size(px(30.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(cx.theme().radius)
        .bg(cx.theme().muted.opacity(0.5))
        .child(agent_brand_icon(agent_id, px(16.0), None))
        .into_any_element()
}

impl ManagementCenter {
    /// The Agent list a Provider or a route group shows on its overview.
    fn render_workspace_agent_list(
        &self,
        id_prefix: &'static str,
        rows: Vec<WorkspaceAgentRow>,
        on_set_default: Rc<dyn Fn(&mut ManagementCenter, String, &mut Context<ManagementCenter>)>,
        on_toggle: Rc<
            dyn Fn(
                &mut ManagementCenter,
                String,
                bool,
                &mut Window,
                &mut Context<ManagementCenter>,
            ),
        >,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pending = self.mutation.is_some();
        if rows.is_empty() {
            return ws_empty_line(
                management_locale_text(
                    "No Agent can use a model source yet. Add an Agent first.",
                    "还没有可以使用模型来源的 Agent，请先添加 Agent。",
                    "還沒有可以使用模型來源的 Agent，請先新增 Agent。",
                ),
                cx,
            );
        }
        let mut list = v_flex().w_full().min_w_0();
        for (index, row) in rows.into_iter().enumerate() {
            let agent_id = row.agent_id.clone();
            let on_toggle = on_toggle.clone();
            let on_set_default = on_set_default.clone();
            let default_agent = agent_id.clone();
            let toggle_agent = agent_id.clone();
            let bound = row.bound;
            let trailing = h_flex()
                .items_center()
                .gap_2()
                .when(bound && !row.is_default, |trailing| {
                    trailing.child(
                        Button::new(SharedString::from(format!(
                            "{id_prefix}-agent-default-{agent_id}"
                        )))
                        .small()
                        .ghost()
                        .label(management_set_default_label())
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            on_set_default(this, default_agent.clone(), cx)
                        })),
                    )
                })
                .child(
                    Switch::new(SharedString::from(format!(
                        "{id_prefix}-agent-switch-{agent_id}"
                    )))
                    .small()
                    .checked(bound)
                    .disabled(pending)
                    .accessibility_label(SharedString::from(row.label.clone()))
                    .on_click(cx.listener(
                        move |this, checked, window, cx| {
                            on_toggle(this, toggle_agent.clone(), *checked, window, cx)
                        },
                    )),
                )
                .into_any_element();
            list = list.child(ws_row(
                SharedString::from(format!("{id_prefix}-agent-{agent_id}")),
                ws_agent_mark(&agent_id, cx),
                h_flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .child(div().min_w_0().truncate().text_sm().child(row.label))
                    .when(row.is_default, |title| {
                        title.child(
                            Tag::color(ColorName::Neutral)
                                .xsmall()
                                .child(management_locale_text("Default", "默认", "預設")),
                        )
                    })
                    .into_any_element(),
                Some(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if bound {
                            management_locale_text("Using it", "正在使用", "正在使用")
                        } else {
                            management_locale_text("Not using it", "未使用", "未使用")
                        })
                        .into_any_element(),
                ),
                trailing,
                index == 0,
                cx,
            ));
        }
        list.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    const SIDEBAR: &str = include_str!("sidebar.rs");
    const PROVIDER: &str = include_str!("provider.rs");
    const ROUTE_GROUP: &str = include_str!("route_group.rs");
    const GATEWAY: &str = include_str!("gateway.rs");
    const CREATE: &str = include_str!("create.rs");

    /// The left column lists every record the page owns at once: the gateway,
    /// then foldable Provider and route-group sections, with no tab bar that
    /// hides half of them.
    #[test]
    fn the_left_column_lists_gateway_providers_and_groups_together() {
        assert!(!SIDEBAR.contains("TabBar::new"));
        let gateway = SIDEBAR.find("render_workspace_gateway_row(cx)").unwrap();
        let providers = SIDEBAR.find("management-workspace-providers-head").unwrap();
        let routes = SIDEBAR.find("management-workspace-routes-head").unwrap();
        assert!(gateway < providers && providers < routes);
        assert!(SIDEBAR.contains("workspace_providers_folded"));
        assert!(SIDEBAR.contains("workspace_routes_folded"));
        // One New command adds any record, and import sits beside it.
        assert!(SIDEBAR.contains("open_provider_create_page(None, window, cx)"));
        assert!(SIDEBAR.contains("open_route_group_page_editor(window, cx)"));
        assert!(SIDEBAR.contains("import_shared_providers("));
    }

    /// Every detail view opens with the same identity header and underline
    /// tabs, so the three records read as one page family.
    #[test]
    fn every_detail_view_shares_the_header_and_tabs() {
        for (name, source) in [
            ("provider", PROVIDER),
            ("route group", ROUTE_GROUP),
            ("gateway", GATEWAY),
        ] {
            assert!(
                source.contains("ws_page_header("),
                "the {name} page has the header"
            );
            assert!(
                source.contains("ws_tabs("),
                "the {name} page has underline tabs"
            );
            assert!(
                !source.contains(".segmented()"),
                "the {name} page has no segmented tabs"
            );
        }
        assert!(super::ws_tabs_are_underlined());
    }

    /// Agents bind and unbind through a switch on their own row; nothing on
    /// the list unbinds on a stray click.
    #[test]
    fn agent_lists_use_switches_for_binding() {
        let kit = include_str!("mod.rs");
        let list = kit
            .split_once("fn render_workspace_agent_list(")
            .map(|(_, tail)| tail)
            .unwrap();
        assert!(list.contains("Switch::new("));
        assert!(list.contains("on_toggle(this, toggle_agent.clone(), *checked, window, cx)"));
        assert!(PROVIDER.contains("try_unbind_model_source("));
        assert!(ROUTE_GROUP.contains("try_unbind_model_source("));
    }

    /// The Provider settings form is the edit mode: it loads from the record
    /// on screen and reloads when that record's revision changes.
    #[test]
    fn provider_settings_reload_from_the_record_revision() {
        assert!(PROVIDER.contains("self.workspace_edit_revision != Some(profile.revision)"));
        assert!(PROVIDER.contains("self.open_shared_provider_editor(&profile, window, cx)"));
        assert!(PROVIDER.contains("save_shared_provider_editor(profile_id.clone(), cx)"));
    }

    /// Editing a group from its page stays on the page instead of opening the
    /// Agent dialog over it.
    #[test]
    fn route_groups_edit_on_their_page() {
        assert!(ROUTE_GROUP.contains("edit_route_group_on_page("));
        assert!(!ROUTE_GROUP.contains("this.edit_route_group(route_id"));
    }

    /// New Provider starts at the preset gallery, and a preset moves the page
    /// to its form.
    #[test]
    fn creating_starts_at_the_gallery() {
        assert!(CREATE.contains("if self.workspace_create_choosing"));
        assert!(CREATE.contains("this.apply_provider_create_preset(&preset_id, window, cx);"));
        assert!(CREATE.contains("this.clear_provider_create_preset(window, cx);"));
        assert!(CREATE.contains("save_provider_create_page(cx)"));
    }
}

/// Whether the shared tab bar uses the underline variant.
#[cfg(test)]
fn ws_tabs_are_underlined() -> bool {
    include_str!("mod.rs")
        .split_once("fn ws_tabs(")
        .is_some_and(|(_, tail)| {
            tail.split_once("\n}\n")
                .is_some_and(|(body, _)| body.contains(".underline()"))
        })
}

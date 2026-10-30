//! One route group's page, and the page that authors one.
//!
//! A route group is one model name that tries several Provider models in an
//! order. Its page leads with that order: who answers first, what happens on
//! failure, and which Agents ask for it.

use super::*;

const ROUTE_TABS: [RouteGroupDetailTab; 3] = [
    RouteGroupDetailTab::Overview,
    RouteGroupDetailTab::Members,
    RouteGroupDetailTab::Traffic,
];

fn route_tab_label(tab: RouteGroupDetailTab) -> &'static str {
    match tab {
        RouteGroupDetailTab::Members => management_locale_text("Members", "成员", "成員"),
        RouteGroupDetailTab::Traffic => management_locale_text("Traffic", "流量", "流量"),
        _ => management_locale_text("Overview", "概览", "概覽"),
    }
}

impl ManagementCenter {
    pub(super) fn render_workspace_route_group(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = shared_route_group_rows(&self.model_routes, &self.agent_model_route_bindings);
        let Some(row) = rows
            .into_iter()
            .find(|row| Some(row.route.id.as_str()) == self.selected_global_route_id.as_deref())
        else {
            return detail_empty_state(
                management_locale_text("No route group selected", "未选择路由组", "未選擇路由組"),
                management_locale_text(
                    "Pick one on the left, or create one from New.",
                    "在左侧选择一个，或通过“新建”创建。",
                    "在左側選擇一個，或透過「新建」建立。",
                ),
                cx,
            );
        };
        let tab = self.route_group_detail_tab;
        let selected = ROUTE_TABS.iter().position(|item| *item == tab).unwrap_or(0);
        let body = match tab {
            RouteGroupDetailTab::Members => self.render_workspace_route_members(&row.route, cx),
            RouteGroupDetailTab::Traffic => self.render_workspace_route_traffic(&row.route, cx),
            _ => self.render_workspace_route_overview(&row, cx),
        };
        v_flex()
            .id("management-route-group-page")
            .w_full()
            .min_w_0()
            .gap_4()
            .child(self.render_workspace_route_header(&row, cx))
            .child(ws_tabs(
                "management-route-group-detail-tabs",
                ROUTE_TABS.iter().map(|tab| route_tab_label(*tab)).collect(),
                selected,
                |this, index, _, cx| {
                    if let Some(tab) = ROUTE_TABS.get(index) {
                        this.route_group_detail_tab = *tab;
                        cx.notify();
                    }
                },
                cx,
            ))
            .child(body)
            .into_any_element()
    }

    fn render_workspace_route_header(
        &self,
        row: &SharedRouteGroupRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let route = &row.route;
        let pending = self.mutation.is_some();
        let enabled_members = route.members.iter().filter(|member| member.enabled).count();
        let mut badges = Vec::new();
        if route.status != vibex_core::RouteStatus::Enabled {
            badges.push(
                Tag::color(ColorName::Neutral)
                    .xsmall()
                    .child(management_locale_text("Off", "已停用", "已停用"))
                    .into_any_element(),
            );
        }
        if route.unlists || route.sink {
            badges.push(
                Tag::color(ColorName::Neutral)
                    .xsmall()
                    .child(management_locale_text("Hidden", "已隐藏", "已隱藏"))
                    .into_any_element(),
            );
        }
        let facts = ws_facts(
            vec![
                ws_mono_fact(route.display_name.clone(), cx),
                div()
                    .child(format!(
                        "{enabled_members}/{} {}",
                        route.members.len(),
                        management_locale_text("members on", "个成员启用", "個成員啟用")
                    ))
                    .into_any_element(),
                div()
                    .child(management_route_strategy_label(route.strategy))
                    .into_any_element(),
            ],
            cx,
        );
        let route_id = route.id.clone();
        let center = cx.weak_entity();
        let menu_route = route.id.clone();
        let more = Button::new("management-route-group-more")
            .small()
            .ghost()
            .icon(IconName::Ellipsis)
            .tooltip(management_locale_text(
                "More actions",
                "更多操作",
                "更多操作",
            ))
            .accessibility_label(management_locale_text(
                "More actions",
                "更多操作",
                "更多操作",
            ))
            .disabled(pending)
            .dropdown_menu(move |menu, _, _| {
                let clone = {
                    let center = center.clone();
                    let id = menu_route.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| this.clone_route_group(id.clone(), cx))
                            .ok();
                    }
                };
                let delete = {
                    let center = center.clone();
                    let id = menu_route.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.delete_route_group(id.clone(), window, cx)
                            })
                            .ok();
                    }
                };
                menu.item(
                    PopupMenuItem::new(management_locale_text("Duplicate", "复制一份", "複製一份"))
                        .icon(IconName::Copy)
                        .on_click(clone),
                )
                .separator()
                .item(
                    PopupMenuItem::new(management_locale_text(
                        "Delete route group…",
                        "删除路由组…",
                        "刪除路由組…",
                    ))
                    .icon(Icon::default().path("icons/vibex/trash-2.svg"))
                    .on_click(delete),
                )
            });
        let actions = h_flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("management-route-group-page-edit")
                    .small()
                    .outline()
                    .icon(Icon::default().path("icons/vibex/pencil.svg"))
                    .label(management_locale_text("Edit…", "编辑…", "編輯…"))
                    .disabled(pending)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.edit_route_group_on_page(route_id.clone(), window, cx)
                    })),
            )
            .child(more)
            .into_any_element();
        ws_page_header(
            ws_icon_tile(
                "icons/vibex/git-branch.svg",
                px(48.0),
                Some(cx.theme().primary),
                cx,
            ),
            ws_title(route.display_name.clone(), badges),
            facts,
            actions,
        )
    }

    fn render_workspace_route_overview(
        &mut self,
        row: &SharedRouteGroupRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let route = &row.route;
        let models = management_route_group_models(route, &self.providers);
        let providers = management_route_group_provider_names(route, &self.providers);
        let tiles = ws_tile_row(vec![
            management_stat_tile(
                management_locale_text("Members", "成员", "成員"),
                route
                    .members
                    .iter()
                    .filter(|member| member.enabled)
                    .count()
                    .to_string(),
                format!(
                    "{} {}",
                    providers.len(),
                    management_locale_text("Providers", "个供应商", "個供應商")
                ),
                None,
                cx,
            ),
            management_stat_tile(
                management_locale_text("Models", "模型", "模型"),
                models.len().to_string(),
                management_locale_text(
                    "Names Agents can ask for",
                    "Agent 可请求的模型名",
                    "Agent 可請求的模型名",
                )
                .to_string(),
                models.is_empty().then(|| cx.theme().warning),
                cx,
            ),
            management_stat_tile(
                "Agent",
                row.agents.len().to_string(),
                management_locale_text("Using this group", "正在使用", "正在使用").to_string(),
                None,
                cx,
            ),
        ]);

        // The order, as a short read-only pipeline: the full table is the
        // Members tab.
        let mut order = v_flex().w_full().min_w_0();
        for (index, member) in route.members.iter().enumerate() {
            order = order.child(self.render_workspace_route_member_row(index, member, cx));
        }
        if route.members.is_empty() {
            order = order.child(ws_empty_line(
                management_locale_text(
                    "No member yet. Edit the group to add Provider models.",
                    "还没有成员。编辑路由组即可添加供应商模型。",
                    "還沒有成員。編輯路由組即可新增供應商模型。",
                ),
                cx,
            ));
        }

        // Agents.
        let route_id = route.id.clone();
        let mut agents = Vec::new();
        for agent_id in &row.agents {
            let is_default = self.resolved_agent_default_source(agent_id).as_ref()
                == Some(&vibex_core::AgentModelSourceRef::RouteGroup(
                    route.id.clone(),
                ));
            agents.push(WorkspaceAgentRow {
                agent_id: agent_id.clone(),
                label: self.workspace_agent_label(agent_id),
                bound: true,
                is_default,
            });
        }
        for (agent_id, label) in self.workspace_configurable_agents() {
            if !row.agents.contains(&agent_id) {
                agents.push(WorkspaceAgentRow {
                    agent_id,
                    label,
                    bound: false,
                    is_default: false,
                });
            }
        }
        let set_default: Rc<dyn Fn(&mut ManagementCenter, String, &mut Context<ManagementCenter>)> =
            Rc::new({
                let route_id = route_id.clone();
                move |this, agent_id, cx| {
                    this.set_agent_default_model_source(
                        agent_id,
                        vibex_core::AgentModelSourceRef::RouteGroup(route_id.clone()),
                        cx,
                    )
                }
            });
        let toggle: Rc<
            dyn Fn(
                &mut ManagementCenter,
                String,
                bool,
                &mut Window,
                &mut Context<ManagementCenter>,
            ),
        > = Rc::new(move |this, agent_id, bind, window, cx| {
            if bind {
                if let Ok(agent) = AgentId::parse(agent_id) {
                    this.bind_route_group(agent, route_id.clone(), cx);
                }
            } else {
                let label = this.workspace_agent_label(&agent_id);
                let Ok(agent) = AgentId::parse(agent_id) else {
                    return;
                };
                this.try_unbind_model_source(
                    agent,
                    vibex_core::AgentModelSourceRef::RouteGroup(route_id.clone()),
                    UnbindModelSource::Route(route_id.clone()),
                    label,
                    window,
                    cx,
                );
            }
        });
        let agent_list = self.render_workspace_agent_list(
            "management-route-group",
            agents,
            set_default,
            toggle,
            cx,
        );

        // Policy, read-only; Edit changes it.
        let pin = match route.pin {
            vibex_core::GatewayPinScope::Off => {
                management_locale_text("Chosen per request", "每次请求重新选择", "每次請求重新選擇")
            }
            vibex_core::GatewayPinScope::Conversation => management_locale_text(
                "Kept for the whole conversation",
                "整个对话保持同一个",
                "整個對話保持同一個",
            ),
        };
        let failover = match route.failover {
            vibex_core::GatewayFailover::Auto => {
                management_locale_text("Switch automatically", "自动切换", "自動切換")
            }
            vibex_core::GatewayFailover::Ask => {
                management_locale_text("Ask first", "先询问", "先詢問")
            }
        };
        let value = |text: String| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text)
                .into_any_element()
        };
        let policy = management_settings_list(
            vec![
                management_setting_row(
                    management_locale_text("Strategy", "策略", "策略"),
                    management_locale_text(
                        "How the next member is chosen.",
                        "如何选择下一个成员。",
                        "如何選擇下一個成員。",
                    ),
                    value(management_route_strategy_label(route.strategy)),
                    cx,
                ),
                management_setting_row(
                    management_locale_text("Session pinning", "会话固定", "會話固定"),
                    management_locale_text(
                        "Vendors cache a prompt per account; moving a conversation loses it.",
                        "厂商按账号缓存提示词，换成员会丢掉缓存。",
                        "廠商按帳號快取提示詞，換成員會遺失快取。",
                    ),
                    value(pin.to_string()),
                    cx,
                ),
                management_setting_row(
                    management_locale_text("On failure", "失败时", "失敗時"),
                    management_locale_text(
                        "What happens when the current member stops answering.",
                        "当前成员无法响应时的处理方式。",
                        "目前成員無法回應時的處理方式。",
                    ),
                    value(failover.to_string()),
                    cx,
                ),
                management_setting_row(
                    management_locale_text("First token timeout", "首 Token 超时", "首 Token 逾時"),
                    management_locale_text(
                        "How long to wait before moving on.",
                        "等待多久后切换到下一个成员。",
                        "等待多久後切換到下一個成員。",
                    ),
                    value(
                        route
                            .first_token_timeout_ms
                            .map(|ms| format!("{} s", (ms / 1000).max(1)))
                            .unwrap_or_else(|| {
                                management_locale_text("Gateway default", "网关默认", "閘道預設")
                                    .to_string()
                            }),
                    ),
                    cx,
                ),
                management_setting_row(
                    management_locale_text("Token ceiling", "Token 上限", "Token 上限"),
                    management_locale_text(
                        "Counted locally from what the gateway observed.",
                        "由网关按本地观测计数。",
                        "由閘道按本機觀測計數。",
                    ),
                    value(
                        route
                            .token_limit
                            .map(|limit| {
                                format!(
                                    "{} / {}",
                                    gateway_token_count(limit.tokens),
                                    gateway_token_window_label(limit.window)
                                )
                            })
                            .unwrap_or_else(|| {
                                management_locale_text("Not limited", "不限制", "不限制")
                                    .to_string()
                            }),
                    ),
                    cx,
                ),
            ],
            cx,
        );

        let model_chips = h_flex()
            .w_full()
            .flex_wrap()
            .gap_1p5()
            .children(models.iter().map(|model| {
                Tag::color(ColorName::Neutral).child(
                    div()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .child(model.clone()),
                )
            }))
            .when(models.is_empty(), |chips| {
                chips.child(ws_empty_line(
                    management_locale_text(
                        "This group names no model Agents can ask for.",
                        "这个路由组没有 Agent 可请求的模型名。",
                        "這個路由組沒有 Agent 可請求的模型名。",
                    ),
                    cx,
                ))
            });

        ws_stack(vec![
            tiles,
            ws_section(
                management_locale_text("Attempt order", "尝试顺序", "嘗試順序"),
                Some(management_locale_text(
                    "The gateway tries members from the top and moves down when one fails.",
                    "网关从上往下尝试成员，失败时依次切换到下一个。",
                    "閘道從上往下嘗試成員，失敗時依序切換到下一個。",
                )),
                None,
                order.into_any_element(),
                cx,
            ),
            ws_section(
                management_locale_text("Model names", "对外模型名", "對外模型名"),
                Some(management_locale_text(
                    "What an Agent asks for to reach this group.",
                    "Agent 通过这些模型名调用该路由组。",
                    "Agent 透過這些模型名呼叫該路由組。",
                )),
                None,
                model_chips.into_any_element(),
                cx,
            ),
            ws_section(
                management_locale_text("Agents", "Agent", "Agent"),
                Some(management_locale_text(
                    "Turn an Agent on to let it ask for this group's models.",
                    "打开开关即可让 Agent 使用该路由组的模型。",
                    "打開開關即可讓 Agent 使用該路由組的模型。",
                )),
                None,
                agent_list,
                cx,
            ),
            ws_section(
                management_locale_text("Policy", "策略", "策略"),
                None,
                None,
                policy,
                cx,
            ),
        ])
    }

    /// One member, read-only: its place, its Provider and model, and what is
    /// fixed about it.
    fn render_workspace_route_member_row(
        &self,
        index: usize,
        member: &vibex_core::ModelRouteMember,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let profile = self
            .providers
            .iter()
            .find(|profile| profile.id == member.model_provider_profile_id);
        let provider_name = profile
            .map(|profile| profile.display_name.clone())
            .unwrap_or_else(|| {
                management_locale_text("Missing Provider", "供应商已不存在", "供應商已不存在")
                    .to_string()
            });
        let mark = h_flex()
            .flex_none()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(rems(1.25))
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child((index + 1).to_string()),
            )
            .child(match profile {
                Some(profile) => provider_logo_tile(profile, px(30.0), cx),
                None => ws_icon_tile(
                    "icons/vibex/database.svg",
                    px(30.0),
                    Some(cx.theme().warning),
                    cx,
                ),
            })
            .into_any_element();
        let mut tags = h_flex().items_center().gap_1();
        if member.weight > 1 {
            tags = tags.child(
                Tag::color(ColorName::Neutral)
                    .xsmall()
                    .child(format!("×{}", member.weight)),
            );
        }
        if let Some(effort) = member.reasoning_effort.as_deref() {
            tags = tags.child(
                Tag::color(ColorName::Neutral)
                    .xsmall()
                    .child(route_group_effort_label(effort)),
            );
        }
        if !member.enabled {
            tags = tags.child(
                Tag::color(ColorName::Neutral)
                    .xsmall()
                    .child(management_locale_text("Off", "已停用", "已停用")),
            );
        }
        ws_row(
            SharedString::from(format!("management-route-member-{index}")),
            mark,
            div()
                .min_w_0()
                .truncate()
                .text_sm()
                .when(!member.enabled, |text| {
                    text.text_color(cx.theme().muted_foreground)
                })
                .child(provider_name)
                .into_any_element(),
            Some(ws_mono_fact(member.model_id.clone(), cx)),
            tags.into_any_element(),
            index == 0,
            cx,
        )
        .text_xs()
        .into_any_element()
    }

    fn render_workspace_route_members(
        &mut self,
        route: &vibex_core::ModelRoute,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = v_flex().w_full().min_w_0();
        for (index, member) in route.members.iter().enumerate() {
            list = list.child(self.render_workspace_route_member_row(index, member, cx));
        }
        if route.members.is_empty() {
            list = list.child(ws_empty_line(
                management_locale_text(
                    "No member yet. Edit the group to add Provider models.",
                    "还没有成员。编辑路由组即可添加供应商模型。",
                    "還沒有成員。編輯路由組即可新增供應商模型。",
                ),
                cx,
            ));
        }
        let route_id = route.id.clone();
        ws_stack(vec![ws_section(
            management_locale_text("Members", "成员", "成員"),
            Some(management_locale_text(
                "Each member is one model on one Provider, tried in this order.",
                "每个成员是某个供应商上的一个模型，按此顺序尝试。",
                "每個成員是某個供應商上的一個模型，依此順序嘗試。",
            )),
            Some(
                Button::new("management-route-group-members-edit")
                    .small()
                    .outline()
                    .label(management_locale_text(
                        "Edit members…",
                        "编辑成员…",
                        "編輯成員…",
                    ))
                    .disabled(self.mutation.is_some())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.edit_route_group_on_page(route_id.clone(), window, cx)
                    }))
                    .into_any_element(),
            ),
            list.into_any_element(),
            cx,
        )])
    }

    fn render_workspace_route_traffic(
        &mut self,
        route: &vibex_core::ModelRoute,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let members = route
            .members
            .iter()
            .map(|member| member.model_provider_profile_id.clone())
            .collect::<BTreeSet<_>>();
        let cutoff = vibex_core::unix_timestamp_ms() - self.traffic_range.window_ms();
        let traffic = WorkspaceTraffic::collect(self.gateway_requests.iter().filter(|request| {
            members.contains(&request.model_provider_profile_id) && request.created_at_ms >= cutoff
        }));
        ws_stack(vec![ws_section(
            management_locale_text("Traffic", "流量", "流量"),
            Some(management_locale_text(
                "Requests the gateway sent to this group's members. No cost or quota is tracked.",
                "网关发往该组成员的请求；不统计费用或额度。",
                "閘道發往該組成員的請求；不統計費用或額度。",
            )),
            Some(self.workspace_range_menu("management-route-traffic-range", cx)),
            traffic.tiles(cx),
            cx,
        )])
    }

    // ----- Editor ----------------------------------------------------------

    pub(super) fn render_workspace_route_group_editor(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pending = self.mutation.is_some();
        let editing = self.route_group_editing_id.is_some();
        let members = self.route_group_model_members.clone();

        let basics = ws_section(
            management_locale_text("Name", "名称", "名稱"),
            None,
            None,
            ws_field(
                management_locale_text("Model name", "模型名", "模型名"),
                Some(management_locale_text(
                    "Agents ask for this name as if it were a model. Members may use different upstream models.",
                    "Agent 会把它当作模型名来请求；组内成员可以使用不同的上游模型。",
                    "Agent 會把它當作模型名來請求；組內成員可以使用不同的上游模型。",
                )),
                ws_input(
                    &self.route_group_name,
                    false,
                    self.route_group_name_error.as_deref(),
                    cx,
                ),
                cx,
            ),
            cx,
        );

        // Members, editable.
        let mut pipeline = v_flex().w_full().min_w_0();
        for (index, member) in members.iter().enumerate() {
            pipeline = pipeline.child(self.render_workspace_route_member_editor(
                index,
                member,
                members.len(),
                pending,
                cx,
            ));
        }
        if members.is_empty() {
            pipeline = pipeline.child(ws_empty_line(
                management_locale_text(
                    "Pick at least one model below.",
                    "请在下方选择至少一个模型。",
                    "請在下方選擇至少一個模型。",
                ),
                cx,
            ));
        }
        let members_section = ws_section(
            management_locale_text("Members", "成员", "成員"),
            Some(management_locale_text(
                "Tried from the top. Move a member to change its place.",
                "从上往下尝试；移动成员即可调整顺序。",
                "從上往下嘗試；移動成員即可調整順序。",
            )),
            Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{}/{}",
                        members.len(),
                        vibex_core::MAX_ROUTE_MEMBERS
                    ))
                    .into_any_element(),
            ),
            v_flex()
                .w_full()
                .gap_3()
                .child(pipeline)
                .child(Separator::horizontal())
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().muted_foreground)
                        .child(management_locale_text("Add models", "添加模型", "新增模型")),
                )
                .child(self.render_model_group_palette(pending, cx))
                .into_any_element(),
            cx,
        );

        // Strategy.
        let strategy = self.route_group_strategy;
        let strategy_choices = vibex_core::RouteStrategy::ALL
            .into_iter()
            .map(|strategy| (strategy.id(), strategy))
            .collect::<Vec<_>>();
        let cards = ws_choice_cards(
            "management-model-group-strategy",
            vibex_core::RouteStrategy::ALL
                .into_iter()
                .map(|strategy| {
                    (
                        strategy.id(),
                        SharedString::from(management_route_strategy_label(strategy)),
                        Some(SharedString::from(management_route_strategy_guidance(
                            strategy,
                        ))),
                    )
                })
                .collect(),
            Some(strategy.id()),
            pending,
            move |this, value, cx| {
                if let Some((_, strategy)) = strategy_choices.iter().find(|(id, _)| *id == value) {
                    this.route_group_strategy = *strategy;
                    cx.notify();
                }
            },
            cx,
        );
        let mut behaviour = vec![management_setting_row(
            management_locale_text(
                "Keep a conversation on one member",
                "同一对话固定成员",
                "同一對話固定成員",
            ),
            management_locale_text(
                "Vendors cache a prompt per account; moving a conversation loses that cache.",
                "厂商按账号缓存提示词，换成员会丢掉缓存。",
                "廠商按帳號快取提示詞，換成員會遺失快取。",
            ),
            Switch::new("management-model-group-pin")
                .small()
                .checked(self.route_group_pin)
                .disabled(pending)
                .on_click(cx.listener(|this, checked, _, cx| {
                    this.route_group_pin = *checked;
                    cx.notify();
                }))
                .into_any_element(),
            cx,
        )];
        if members.len() >= 2 {
            behaviour.push(management_setting_row(
                management_locale_text("Switch on failure", "失败时自动切换", "失敗時自動切換"),
                management_locale_text(
                    "Move to the next member without asking when the current one fails.",
                    "当前成员失败时，不经询问直接切换到下一个。",
                    "目前成員失敗時，不經詢問直接切換到下一個。",
                ),
                Switch::new("management-model-group-auto-failover")
                    .small()
                    .checked(self.route_group_auto_failover)
                    .disabled(pending)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.route_group_auto_failover = *checked;
                        cx.notify();
                    }))
                    .into_any_element(),
                cx,
            ));
        }
        let strategy_section = ws_section(
            management_locale_text("Strategy", "策略", "策略"),
            Some(management_locale_text(
                "How the gateway picks a member for each request.",
                "网关为每个请求挑选成员的方式。",
                "閘道為每個請求挑選成員的方式。",
            )),
            None,
            v_flex()
                .w_full()
                .gap_3()
                .child(cards)
                .child(management_settings_list(behaviour, cx))
                .into_any_element(),
            cx,
        );

        // Limits.
        let limits = ws_section(
            management_locale_text("Limits", "限制", "限制"),
            Some(management_locale_text(
                "Leave a field empty to use the gateway's default.",
                "留空表示使用网关默认值。",
                "留空表示使用閘道預設值。",
            )),
            None,
            v_flex()
                .w_full()
                .gap_4()
                .child(ws_field_pair(
                    ws_field(
                        management_locale_text(
                            "First token timeout (s)",
                            "首 Token 超时（秒）",
                            "首 Token 逾時（秒）",
                        ),
                        None,
                        ws_input(&self.route_group_first_token_timeout, false, None, cx),
                        cx,
                    ),
                    ws_field(
                        management_locale_text("Patience (s)", "耐心等待（秒）", "耐心等待（秒）"),
                        Some(management_locale_text(
                            "How long to wait for a cooling member before giving up.",
                            "成员冷却时最多等待多久。",
                            "成員冷卻時最多等待多久。",
                        )),
                        ws_input(&self.route_group_patience, false, None, cx),
                        cx,
                    ),
                ))
                .child(ws_field_pair(
                    ws_field(
                        management_locale_text("Context tokens", "上下文 Token", "上下文 Token"),
                        Some(management_locale_text(
                            "Empty uses the smallest member's window.",
                            "留空使用最小成员的上下文窗口。",
                            "留空使用最小成員的上下文視窗。",
                        )),
                        ws_input(&self.route_group_context_tokens, false, None, cx),
                        cx,
                    ),
                    ws_field(
                        management_locale_text(
                            "Token ceiling (local)",
                            "Token 上限（本地）",
                            "Token 上限（本機）",
                        ),
                        None,
                        ws_input(&self.route_group_token_limit, false, None, cx),
                        cx,
                    ),
                ))
                .into_any_element(),
            cx,
        );

        // Visibility.
        let visibility = ws_section(
            management_locale_text("Visibility", "可见性", "可見性"),
            None,
            None,
            management_settings_list(
                vec![
                    management_setting_row(
                        management_locale_text(
                            "Hide from pickers",
                            "不在选择器中显示",
                            "不在選擇器中顯示",
                        ),
                        management_locale_text(
                            "The name stays callable but is not offered in model pickers.",
                            "仍可按名称调用，但不会出现在模型选择器中。",
                            "仍可依名稱呼叫，但不會出現在模型選擇器中。",
                        ),
                        Switch::new("management-model-group-unlists")
                            .small()
                            .checked(self.route_group_unlists)
                            .disabled(pending)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.route_group_unlists = *checked;
                                cx.notify();
                            }))
                            .into_any_element(),
                        cx,
                    ),
                    management_setting_row(
                        management_locale_text("Use as a sink", "作为兜底组合", "作為兜底組合"),
                        management_locale_text(
                            "A hidden composition other groups can fall back to.",
                            "隐藏的组合，供其他路由组兜底使用。",
                            "隱藏的組合，供其他路由組兜底使用。",
                        ),
                        Switch::new("management-model-group-sink")
                            .small()
                            .checked(self.route_group_sink)
                            .disabled(pending)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.route_group_sink = *checked;
                                cx.notify();
                            }))
                            .into_any_element(),
                        cx,
                    ),
                ],
                cx,
            ),
            cx,
        );

        let simulator = ws_section(
            management_locale_text("Try it", "试运行", "試執行"),
            Some(management_locale_text(
                "See which member would answer a request, without sending one.",
                "不发送请求，查看哪个成员会响应。",
                "不傳送請求，查看哪個成員會回應。",
            )),
            None,
            self.render_model_group_simulator(&members, pending, cx),
            cx,
        );

        let enabled = members.iter().filter(|member| member.enabled).count();
        let footer = ws_form_footer(
            None,
            vec![
                Button::new("management-model-group-cancel")
                    .ghost()
                    .label(management_locale_text("Cancel", "取消", "取消"))
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_route_group_editor(cx);
                        if window.has_active_dialog(cx) {
                            window.close_dialog(cx);
                        }
                    }))
                    .into_any_element(),
                Button::new("management-model-group-save")
                    .primary()
                    .label(if editing {
                        management_locale_text("Save", "保存", "儲存")
                    } else {
                        management_locale_text("Create route group", "创建路由组", "建立路由組")
                    })
                    .loading(pending)
                    .disabled(pending || enabled == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.save_route_group(cx)))
                    .into_any_element(),
            ],
            cx,
        );

        let back = Button::new("management-route-group-page-back")
            .small()
            .ghost()
            .icon(IconName::ArrowLeft)
            .label(management_locale_text("Back", "返回", "返回"))
            .disabled(pending)
            .on_click(cx.listener(|this, _, _, cx| this.close_route_group_editor(cx)))
            .into_any_element();
        ws_authoring_page(
            "management-route-group-page-editor",
            back,
            Some(ws_icon_tile(
                "icons/vibex/git-branch.svg",
                px(44.0),
                Some(cx.theme().primary),
                cx,
            )),
            if editing {
                management_locale_text("Edit route group", "编辑路由组", "編輯路由組")
            } else {
                management_locale_text("New route group", "新建路由组", "新增路由組")
            },
            management_locale_text(
                "Combine models from several Providers under one name, with an order to fall back through.",
                "把多个供应商的模型合成一个模型名，并按顺序依次回退。",
                "把多個供應商的模型合成一個模型名，並依順序依次回退。",
            ),
            ws_stack(vec![
                basics,
                members_section,
                strategy_section,
                limits,
                visibility,
                simulator,
                footer,
            ]),
            cx,
        )
    }

    /// One editable member: its place, identity, and the controls that change
    /// it, in fixed lanes so the rows line up.
    fn render_workspace_route_member_editor(
        &self,
        index: usize,
        member: &vibex_core::ModelRouteMember,
        count: usize,
        pending: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let profile = self
            .providers
            .iter()
            .find(|profile| profile.id == member.model_provider_profile_id);
        let provider_name = profile
            .map(|profile| profile.display_name.clone())
            .unwrap_or_else(|| member.model_provider_profile_id.as_str().to_string());
        let effort = member.reasoning_effort.clone().unwrap_or_default();
        let center = cx.weak_entity();
        let effort_menu = Button::new(SharedString::from(format!(
            "management-model-group-effort-{index}"
        )))
        .xsmall()
        .ghost()
        .label(route_group_effort_label(&effort))
        .dropdown_caret(true)
        .disabled(pending)
        .dropdown_menu(move |mut menu, _, _| {
            for (value, label) in [
                (
                    "",
                    management_locale_text("Inherit effort", "继承推理档位", "繼承推理檔位"),
                ),
                ("low", management_locale_text("Low", "低", "低")),
                ("medium", management_locale_text("Medium", "中", "中")),
                ("high", management_locale_text("High", "高", "高")),
                (
                    "xhigh",
                    management_locale_text("Extra high", "极高", "極高"),
                ),
                ("max", management_locale_text("Max", "最高", "最高")),
            ] {
                let center = center.clone();
                let checked = value == effort;
                menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                    move |_, _, cx| {
                        center
                            .update(cx, |this, cx| {
                                this.set_route_group_model_effort(index, value.to_string(), cx)
                            })
                            .ok();
                    },
                ));
            }
            menu
        });
        let mark = h_flex()
            .flex_none()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(rems(1.25))
                    .text_xs()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child((index + 1).to_string()),
            )
            .child(match profile {
                Some(profile) => provider_logo_tile(profile, px(30.0), cx),
                None => ws_icon_tile(
                    "icons/vibex/database.svg",
                    px(30.0),
                    Some(cx.theme().warning),
                    cx,
                ),
            })
            .into_any_element();
        let trailing =
            h_flex()
                .items_center()
                .gap_1()
                .child(
                    Button::new(SharedString::from(format!(
                        "management-model-group-weight-{index}"
                    )))
                    .xsmall()
                    .ghost()
                    .label(format!("×{}", member.weight))
                    .tooltip(management_locale_text(
                        "Weight: click to change",
                        "权重：点击切换",
                        "權重：點擊切換",
                    ))
                    .disabled(pending)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cycle_route_group_model_weight(index, cx)
                    })),
                )
                .child(effort_menu)
                .child(
                    Switch::new(SharedString::from(format!(
                        "management-model-group-enabled-{index}"
                    )))
                    .small()
                    .checked(member.enabled)
                    .disabled(pending)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_route_group_model_enabled(index, cx)
                    })),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "management-model-group-up-{index}"
                    )))
                    .xsmall()
                    .ghost()
                    .icon(IconName::ArrowUp)
                    .tooltip(management_locale_text("Move up", "上移", "上移"))
                    .disabled(pending || index == 0)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.move_route_group_model_member(index, true, cx)
                    })),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "management-model-group-down-{index}"
                    )))
                    .xsmall()
                    .ghost()
                    .icon(IconName::ArrowDown)
                    .tooltip(management_locale_text("Move down", "下移", "下移"))
                    .disabled(pending || index + 1 >= count)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.move_route_group_model_member(index, false, cx)
                    })),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "management-model-group-remove-{index}"
                    )))
                    .xsmall()
                    .ghost()
                    .icon(Icon::default().path("icons/vibex/trash-2.svg"))
                    .tooltip(management_locale_text(
                        "Remove member",
                        "移除成员",
                        "移除成員",
                    ))
                    .accessibility_label(management_locale_text(
                        "Remove member",
                        "移除成员",
                        "移除成員",
                    ))
                    .disabled(pending)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_route_group_model_member(index, cx)
                    })),
                )
                .into_any_element();
        ws_row(
            SharedString::from(format!("management-model-group-member-{index}")),
            mark,
            div()
                .min_w_0()
                .truncate()
                .text_sm()
                .when(!member.enabled, |text| {
                    text.text_color(cx.theme().muted_foreground)
                })
                .child(provider_name)
                .into_any_element(),
            Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(ws_mono_fact(member.model_id.clone(), cx))
                    .into_any_element(),
            ),
            trailing,
            index == 0,
            cx,
        )
        .into_any_element()
    }
}

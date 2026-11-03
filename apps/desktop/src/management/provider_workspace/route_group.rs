//! One route group's page, the page that authors one, and the form both
//! share with the Agent dialog.
//!
//! A route group is one model name that tries several Provider models in an
//! order. Its page reads like a Provider's: an identity header with the
//! switch that turns it on, then tabs — Overview (who answers, who asks, and
//! the traffic they produced), Members (the order, edited in place) and
//! Settings (how it picks and its limits). The new-group page and the Agent
//! dialog draw the very same sections, so a setting found in one entry is
//! found, worded the same, in every other.

use super::*;

const ROUTE_TABS: [RouteGroupDetailTab; 3] = [
    RouteGroupDetailTab::Overview,
    RouteGroupDetailTab::Members,
    RouteGroupDetailTab::Settings,
];

fn route_tab_label(tab: RouteGroupDetailTab) -> &'static str {
    match tab {
        RouteGroupDetailTab::Members => management_locale_text("Members", "成员", "成員"),
        RouteGroupDetailTab::Settings => management_locale_text("Settings", "设置", "設定"),
        _ => management_locale_text("Overview", "概览", "概覽"),
    }
}

/// Where the shared route-group form is drawn; only its closing commands
/// differ between the three.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::management) enum RouteGroupFormScope {
    /// The Provider page's new-group page.
    Page,
    /// The Agent's route-group dialog.
    Dialog,
    /// A saved group's Members or Settings tab.
    Detail,
}

impl ManagementCenter {
    pub(super) fn render_workspace_route_group(
        &mut self,
        window: &mut Window,
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
        if matches!(
            tab,
            RouteGroupDetailTab::Members | RouteGroupDetailTab::Settings
        ) {
            self.ensure_workspace_route_draft(&row.route, window, cx);
        }
        let body = match tab {
            RouteGroupDetailTab::Members => self.render_workspace_route_members(cx),
            RouteGroupDetailTab::Settings => self.render_workspace_route_settings(cx),
            _ => self.render_workspace_route_overview(&row, cx),
        };
        let header = self.render_workspace_route_header(&row, cx);
        let tabs = ws_tabs(
            "management-route-group-detail-tabs",
            ROUTE_TABS.iter().map(|tab| route_tab_label(*tab)).collect(),
            selected,
            |this, index, _, cx| {
                if let Some(tab) = ROUTE_TABS.get(index) {
                    this.route_group_detail_tab = *tab;
                    this.workspace_detail_scroll
                        .set_offset(point(px(0.0), px(0.0)));
                    this.workspace_route_members_scroll
                        .set_offset(point(px(0.0), px(0.0)));
                    cx.notify();
                }
            },
            cx,
        );
        if tab == RouteGroupDetailTab::Members {
            ws_detail_page_fixed("management-route-group-page", header, tabs, body, cx)
        } else {
            ws_detail_page("management-route-group-page", header, tabs, body, cx)
        }
    }

    /// Loads the group on screen into the shared draft, the way a Provider's
    /// Settings form loads its record: on first view, and again whenever the
    /// record's revision moves underneath it.
    fn ensure_workspace_route_draft(
        &mut self,
        route: &vibex_core::ModelRoute,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A dialog that is authoring a group owns the draft until it closes.
        if self.route_group_editor_open {
            return;
        }
        if self.route_group_editing_id.as_ref() == Some(&route.id)
            && self.workspace_route_edit_revision == Some(route.revision)
        {
            return;
        }
        if self.load_route_group_draft(route.id.clone(), false, window, cx) {
            // Loading prepares the dialog's draft; on the page nothing is
            // presented, so the dialog flag must not stay raised.
            self.route_group_editor_open = false;
            self.workspace_route_edit_revision = Some(route.revision);
        }
    }

    fn render_workspace_route_header(
        &self,
        row: &SharedRouteGroupRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let route = &row.route;
        let pending = self.mutation.is_some();
        let enabled = route.status == vibex_core::RouteStatus::Enabled;
        let enabled_members = route.members.iter().filter(|member| member.enabled).count();
        let mut badges = Vec::new();
        if !enabled {
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
        // The same pair of commands a Provider's header holds: the switch
        // that says whether the record answers at all, then the overflow.
        let actions = h_flex()
            .items_center()
            .gap_2()
            .child(
                Switch::new("management-route-group-enabled")
                    .small()
                    .checked(enabled)
                    .disabled(pending)
                    .label(management_locale_text("Enabled", "启用", "啟用"))
                    .on_click(cx.listener({
                        let id = route.id.clone();
                        move |this, checked, _, cx| {
                            this.set_route_group_enabled(id.clone(), *checked, cx)
                        }
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

    /// Turns a saved group on or off without touching anything else on it.
    fn set_route_group_enabled(
        &mut self,
        route_id: vibex_core::ModelRouteId,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        if self.mutation.is_some() {
            return;
        }
        let Some(mut route) = self
            .model_routes
            .iter()
            .find(|route| route.id == route_id)
            .cloned()
        else {
            return;
        };
        let expected_revision = route.revision;
        route.status = if enabled {
            vibex_core::RouteStatus::Enabled
        } else {
            vibex_core::RouteStatus::Disabled
        };
        self.error = None;
        self.mutation = Some(ManagementMutation::Gateway);
        let this = cx.entity();
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = backend
                .management()
                .update_model_route(vibex_backend::MutationRequest::new(
                    vibex_core::ModelRouteUpdateRequest {
                        route,
                        expected_revision,
                    },
                ))
                .await;
            this.update(cx, |this, cx| {
                this.mutation = None;
                match outcome {
                    Ok(_) => this.refresh(cx),
                    Err(error) => {
                        this.error = Some(format!("{}: {}", error.code, error.message));
                        cx.notify();
                    }
                }
            });
        }));
    }

    /// Enables or disables this route group for every Agent at once.
    ///
    /// The capsules stay the precise control; this is the same answer for the
    /// whole list. Disabling keeps a binding when it is that Agent's only model
    /// source — the guard a single capsule applies — and reports how many were
    /// kept instead of stopping the batch at the first Agent.
    fn set_workspace_route_agents(
        &mut self,
        route_id: vibex_core::ModelRouteId,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        if self.mutation.is_some() {
            return;
        }
        if !self.model_routes.iter().any(|route| route.id == route_id) {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let active_locale = locale::current_locale();
        let mut kept = 0usize;
        let mut requests = Vec::new();
        if enabled {
            let bound = self
                .agent_model_route_bindings
                .iter()
                .filter(|binding| {
                    binding.deleted_at_ms.is_none() && binding.model_route_id == route_id
                })
                .map(|binding| binding.agent_id.as_str().to_string())
                .collect::<BTreeSet<_>>();
            for (agent_id, _) in self.workspace_configurable_agents() {
                if bound.contains(&agent_id) {
                    continue;
                }
                let Ok(agent) = AgentId::parse(agent_id.clone()) else {
                    continue;
                };
                let make_default = self.resolved_agent_default_source(&agent_id).is_none();
                requests.push((agent, make_default));
            }
        } else {
            for binding in self.agent_model_route_bindings.iter().filter(|binding| {
                binding.deleted_at_ms.is_none() && binding.model_route_id == route_id
            }) {
                let agent_id = binding.agent_id.clone();
                if self.agent_model_source_count(agent_id.as_str()) <= 1 {
                    kept += 1;
                    continue;
                }
                requests.push((agent_id, false));
            }
        }
        if requests.is_empty() {
            if kept > 0 {
                self.error = Some(match active_locale {
                    ResolvedLocale::En => format!(
                        "This route group is the only model source for {kept} Agent(s), so it cannot be removed."
                    ),
                    ResolvedLocale::ZhCn => {
                        format!("该路由组是 {kept} 个 Agent 唯一的模型来源，无法移除。")
                    }
                    ResolvedLocale::ZhTw => {
                        format!("該路由組是 {kept} 個 Agent 唯一的模型來源，無法移除。")
                    }
                });
                cx.notify();
            }
            return;
        }

        self.mutation = Some(ManagementMutation::Gateway);
        self.error = None;
        let this = cx.entity();
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let mut applied = 0usize;
            let mut failure = None;
            for (agent_id, make_default) in requests {
                let outcome = if enabled {
                    backend
                        .management()
                        .bind_agent_model_route(vibex_backend::MutationRequest::new(
                            vibex_core::AgentModelRouteBindRequest {
                                agent_id,
                                model_route_id: route_id.clone(),
                                make_default,
                            },
                        ))
                        .await
                        .map(|_| ())
                } else {
                    backend
                        .management()
                        .unbind_agent_model_route(vibex_backend::MutationRequest::new(
                            vibex_core::AgentModelRouteUnbindRequest {
                                agent_id,
                                model_route_id: route_id.clone(),
                            },
                        ))
                        .await
                        .map(|_| ())
                };
                match outcome {
                    Ok(()) => applied += 1,
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            this.update(cx, |this, cx| {
                this.mutation = None;
                this.refresh(cx);
                if let Some(error) = failure {
                    this.error = Some(format!("{}: {}", error.code, error.message));
                } else {
                    let mut message = match active_locale {
                        ResolvedLocale::En => {
                            if enabled {
                                format!("Enabled for {applied} Agent(s)")
                            } else {
                                format!("Disabled for {applied} Agent(s)")
                            }
                        }
                        ResolvedLocale::ZhCn => {
                            if enabled {
                                format!("已为 {applied} 个 Agent 启用")
                            } else {
                                format!("已为 {applied} 个 Agent 取消")
                            }
                        }
                        ResolvedLocale::ZhTw => {
                            if enabled {
                                format!("已為 {applied} 個 Agent 啟用")
                            } else {
                                format!("已為 {applied} 個 Agent 取消")
                            }
                        }
                    };
                    if kept > 0 {
                        let suffix = match active_locale {
                            ResolvedLocale::En => {
                                format!("; {kept} kept because this is their only model source.")
                            }
                            ResolvedLocale::ZhCn => {
                                format!("；{kept} 个 Agent 保留，因为这是它们唯一的模型来源。")
                            }
                            ResolvedLocale::ZhTw => {
                                format!("；{kept} 個 Agent 保留，因為這是它們唯一的模型來源。")
                            }
                        };
                        message.push_str(&suffix);
                    }
                    this.notice = Some(message);
                }
                cx.notify();
            });
        }));
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
                management_locale_text("Strategy", "策略", "策略"),
                management_route_strategy_label(route.strategy),
                match route.failover {
                    vibex_core::GatewayFailover::Auto => management_locale_text(
                        "Switch on failure",
                        "失败时自动切换",
                        "失敗時自動切換",
                    ),
                    vibex_core::GatewayFailover::Ask => {
                        management_locale_text("Ask on failure", "失败时先询问", "失敗時先詢問")
                    }
                }
                .to_string(),
                None,
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

        // The order, as a short read-only pipeline: the Members tab edits it.
        let mut order = v_flex().w_full().min_w_0();
        for (index, member) in route.members.iter().enumerate() {
            order = order.child(self.render_workspace_route_member_row(index, member, cx));
        }
        if route.members.is_empty() {
            order = order.child(ws_empty_line(
                management_locale_text(
                    "No member yet. Add Provider models on the Members tab.",
                    "还没有成员。可在“成员”标签页添加供应商模型。",
                    "還沒有成員。可在「成員」分頁新增供應商模型。",
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
        let agent_scope = management_agent_scope_actions(
            "management-route-group-agent-scope",
            self.mutation.is_some(),
            {
                let route_id = route.id.clone();
                move |this, enabled, _window, cx| {
                    this.set_workspace_route_agents(route_id.clone(), enabled, cx)
                }
            },
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

        let traffic = self.render_workspace_route_traffic_section(route, cx);

        ws_stack(vec![
            tiles,
            traffic,
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
                    "Click an Agent to let it ask for this group's models.",
                    "点选 Agent 即可让它使用该路由组的模型。",
                    "點選 Agent 即可讓它使用該路由組的模型。",
                )),
                Some(agent_scope),
                agent_list,
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
                Some(profile) => provider_logo_mark(profile, px(24.0), cx),
                None => ws_icon_mark(
                    "icons/vibex/database.svg",
                    px(24.0),
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

    // ----- Members & Settings, edited in place ------------------------------

    /// The Members tab: the attempt order itself, editable where it is read.
    ///
    /// The card fills the page and owns one scroll region for its members and
    /// palette; the form's actions stay pinned below it. Sizing the card to its
    /// content instead let a long Provider list run underneath those actions.
    fn render_workspace_route_members(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let footer = self.render_route_group_form_footer(RouteGroupFormScope::Detail, cx);
        v_flex()
            .id("management-route-members-page")
            .debug_selector(|| "management-route-members-page".to_string())
            .w_full()
            .min_w_0()
            .h_full()
            .min_h_0()
            .gap_4()
            .child(self.render_route_group_members_section(true, cx))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .debug_selector(|| "management-route-members-footer".to_string())
                    .child(footer),
            )
            .into_any_element()
    }

    /// The Settings tab: everything about the group that is not its members.
    fn render_workspace_route_settings(&mut self, cx: &mut Context<Self>) -> AnyElement {
        ws_stack(vec![
            self.render_route_group_name_section(cx),
            self.render_route_group_strategy_section(cx),
            self.render_route_group_limits_section(cx),
            self.render_route_group_visibility_section(cx),
            self.render_route_group_simulator_section(cx),
            self.render_route_group_form_footer(RouteGroupFormScope::Detail, cx),
        ])
    }

    // ----- The shared form ---------------------------------------------------

    /// Every section of the route-group form, in the order each entry reads
    /// them. The new-group page and the Agent dialog draw all of them; a saved
    /// group spreads them across its Members and Settings tabs.
    pub(in crate::management) fn render_route_group_form(
        &mut self,
        scope: RouteGroupFormScope,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        ws_stack(vec![
            self.render_route_group_form_sections(scope, cx),
            self.render_route_group_form_footer(scope, cx),
        ])
    }

    /// Every field of the form, without its closing commands.
    ///
    /// The dialog pins those commands below a scrolling body, so it draws the
    /// sections here and the footer separately; the page and the detail tabs
    /// keep them in one stack so the buttons follow the last field.
    pub(in crate::management) fn render_route_group_form_sections(
        &mut self,
        _scope: RouteGroupFormScope,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        ws_stack(vec![
            self.render_route_group_name_section(cx),
            self.render_route_group_members_section(false, cx),
            self.render_route_group_strategy_section(cx),
            self.render_route_group_limits_section(cx),
            self.render_route_group_visibility_section(cx),
            self.render_route_group_simulator_section(cx),
        ])
    }

    fn render_route_group_name_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        ws_section(
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
        )
    }

    fn render_route_group_members_section(
        &mut self,
        fill: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pending = self.mutation.is_some();
        let mut members = self.route_group_model_members.clone();
        let active_drag = self
            .route_member_order_drag_state
            .clone()
            .filter(|_| cx.has_active_drag());
        if let Some(drag) = active_drag.as_ref() {
            // The list follows the pointer, not the draft: the draft only moves
            // once the row is released, so dragging never writes anything.
            members.sort_by_key(|member| {
                drag.preview_ids
                    .iter()
                    .position(|id| id == member.id.as_str())
                    .unwrap_or(usize::MAX)
            });
        }
        let active_drop_target = self
            .route_member_order_drop_target
            .clone()
            .filter(|_| cx.has_active_drag());
        let mut pipeline = v_flex().w_full().min_w_0();
        for (index, member) in members.iter().enumerate() {
            let member_id = member.id.as_str();
            let drop_after = active_drop_target
                .as_ref()
                .and_then(|target| (target.member_id == member_id).then_some(target.after));
            let reorder_offset = active_drag.as_ref().and_then(|drag| {
                provider_reorder_row_offset(&drag.original_ids, &drag.preview_ids, member_id)
            });
            let reorder_animation_id =
                active_drag
                    .as_ref()
                    .zip(active_drop_target.as_ref())
                    .map(|(drag, target)| {
                        format!(
                            "route-member-reorder-{}-{}-{}-{member_id}",
                            drag.member_id, target.member_id, target.after
                        )
                    });
            pipeline = pipeline.child(self.render_workspace_route_member_editor(
                index,
                member,
                members.len(),
                pending,
                drop_after,
                reorder_offset,
                reorder_animation_id,
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
        let palette = self.render_model_group_palette(pending, cx);
        let content = v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .child(pipeline)
            .child(Separator::horizontal())
            .child(palette);
        let title = management_locale_text("Members", "成员", "成員");
        let description = management_locale_text(
            "Tried from the top. Drag a member to change its place.",
            "从上往下尝试；拖动成员即可调整顺序。",
            "從上往下嘗試；拖動成員即可調整順序。",
        );
        let count = Some(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!(
                    "{}/{} {}",
                    members.len(),
                    vibex_core::MAX_ROUTE_MEMBERS,
                    management_locale_text("members", "个成员", "個成員")
                ))
                .into_any_element(),
        );
        if fill {
            let body = v_flex()
                .w_full()
                .min_w_0()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("management-route-members-scroll")
                        .debug_selector(|| "management-route-members-scroll".to_string())
                        .w_full()
                        .min_h_0()
                        .flex_1()
                        .track_scroll(&self.workspace_route_members_scroll)
                        .overflow_y_scroll()
                        .vertical_scrollbar(&self.workspace_route_members_scroll)
                        .child(content),
                )
                .into_any_element();
            ws_section_fill(
                "management-route-members-section",
                title,
                Some(description),
                count,
                body,
                cx,
            )
        } else {
            ws_section(
                title,
                Some(description),
                count,
                content.into_any_element(),
                cx,
            )
        }
    }

    fn render_route_group_strategy_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
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
        let behaviour = vec![
            management_setting_row(
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
            ),
            // Shown whatever the member count, so the setting does not appear
            // and vanish as members come and go; with one member it simply
            // has nothing to switch to.
            management_setting_row(
                management_locale_text("Switch on failure", "失败时自动切换", "失敗時自動切換"),
                management_locale_text(
                    "Move to the next member without asking when the current one fails. Off asks first.",
                    "当前成员失败时，不经询问直接切换到下一个；关闭则先询问。",
                    "目前成員失敗時，不經詢問直接切換到下一個；關閉則先詢問。",
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
            ),
        ];
        ws_section(
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
        )
    }

    fn render_route_group_limits_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
        let windows: [(&'static str, &'static str); 3] = [
            (
                vibex_core::GatewayTokenWindow::Day.id(),
                management_locale_text("Per day", "每天", "每天"),
            ),
            (
                vibex_core::GatewayTokenWindow::Week.id(),
                management_locale_text("Per week", "每周", "每週"),
            ),
            (
                vibex_core::GatewayTokenWindow::Month.id(),
                management_locale_text("Per month", "每月", "每月"),
            ),
        ];
        let window_menu = management_option_menu(
            "management-model-group-token-window",
            &windows,
            self.route_group_token_limit_window.id(),
            pending,
            cx.weak_entity(),
            |this, value, cx| {
                if let Some(window) = vibex_core::GatewayTokenWindow::ALL
                    .into_iter()
                    .find(|window| window.id() == value)
                {
                    this.route_group_token_limit_window = window;
                    cx.notify();
                }
            },
        );
        ws_section(
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
                        Some(management_locale_text(
                            "How long to wait before moving on.",
                            "等待多久后切换到下一个成员。",
                            "等待多久後切換到下一個成員。",
                        )),
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
                        Some(management_locale_text(
                            "Counted locally from what the gateway observed.",
                            "由网关按本地观测计数。",
                            "由閘道按本機觀測計數。",
                        )),
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(ws_input(
                                &self.route_group_token_limit,
                                false,
                                None,
                                cx,
                            )))
                            .child(window_menu)
                            .into_any_element(),
                        cx,
                    ),
                ))
                .into_any_element(),
            cx,
        )
    }

    fn render_route_group_visibility_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
        ws_section(
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
        )
    }

    fn render_route_group_simulator_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
        let members = self.route_group_model_members.clone();
        ws_section(
            management_locale_text("Try it", "试运行", "試執行"),
            Some(management_locale_text(
                "See which member would answer a request, without sending one.",
                "不发送请求，查看哪个成员会响应。",
                "不傳送請求，查看哪個成員會回應。",
            )),
            None,
            self.render_model_group_simulator(&members, pending, cx),
            cx,
        )
    }

    /// The form's closing commands. Every entry commits with the same Save;
    /// what Cancel means differs: close the page, close the dialog, or drop
    /// the unsaved edits of the group on screen.
    pub(in crate::management) fn render_route_group_form_footer(
        &mut self,
        scope: RouteGroupFormScope,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_route_group_form_footer_with(scope, None, cx)
    }

    /// The same closing commands, with one more at the leading edge.
    ///
    /// The dialog keeps its step-changing control beside the commit, so the
    /// reader's way back and their way forward sit on one bar instead of one
    /// floating above the form.
    pub(in crate::management) fn render_route_group_form_footer_with(
        &mut self,
        scope: RouteGroupFormScope,
        leading: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pending = self.mutation.is_some();
        let editing = self.route_group_editing_id.is_some();
        let enabled = self
            .route_group_model_members
            .iter()
            .filter(|member| member.enabled)
            .count();
        let cancel = match scope {
            RouteGroupFormScope::Detail => Button::new("management-model-group-discard")
                .ghost()
                .label(management_locale_text(
                    "Discard changes",
                    "放弃修改",
                    "放棄修改",
                ))
                .disabled(pending)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.workspace_route_edit_revision = None;
                    cx.notify();
                })),
            RouteGroupFormScope::Page | RouteGroupFormScope::Dialog => {
                Button::new("management-model-group-cancel")
                    .ghost()
                    .label(management_locale_text("Cancel", "取消", "取消"))
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, window, cx| {
                        // The same draft is drawn in the dialog and on the
                        // Provider page, so abandoning it ends both.
                        this.close_route_group_editor(cx);
                        if window.has_active_dialog(cx) {
                            window.close_dialog(cx);
                        }
                    }))
            }
        };
        ws_form_footer_with_leading(
            (scope != RouteGroupFormScope::Page)
                .then(|| self.error.clone())
                .flatten(),
            leading,
            vec![
                cancel.into_any_element(),
                Button::new("management-model-group-save")
                    .primary()
                    .label(if editing {
                        management_locale_text("Save", "保存", "儲存")
                    } else {
                        management_locale_text("Create route group", "创建路由组", "建立路由組")
                    })
                    .loading(pending)
                    .disabled(pending || enabled == 0)
                    .on_click(cx.listener(|this, _, window, cx| this.save_route_group(window, cx)))
                    .into_any_element(),
            ],
            cx,
        )
    }

    // ----- The new-group page ------------------------------------------------

    pub(super) fn render_workspace_route_group_editor(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editing = self.route_group_editing_id.is_some();
        let mark = ws_icon_tile(
            "icons/vibex/git-branch.svg",
            px(44.0),
            Some(cx.theme().primary),
            cx,
        );
        ws_authoring_page(
            "management-route-group-page-editor",
            Some(mark),
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
            self.render_route_group_form(RouteGroupFormScope::Page, cx),
            cx,
        )
    }

    /// The group's traffic, read on the Overview tab.
    ///
    /// It used to have a tab of its own, which made a summary the reader checks
    /// while deciding on the group a separate destination. The range control
    /// and the tiles are unchanged; only their home moved.
    fn render_workspace_route_traffic_section(
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
        let traffic =
            WorkspaceTraffic::collect(self.gateway_traffic_requests.iter().filter(|request| {
                members.contains(&request.model_provider_profile_id)
                    && request.created_at_ms >= cutoff
            }));
        ws_section(
            management_locale_text("Traffic", "流量", "流量"),
            Some(management_locale_text(
                "Requests the gateway sent to this group's members. No cost or quota is tracked.",
                "网关发往该组成员的请求；不统计费用或额度。",
                "閘道發往該組成員的請求；不統計費用或額度。",
            )),
            Some(self.workspace_range_menu("management-route-traffic-range", cx)),
            traffic.tiles(cx),
            cx,
        )
    }

    /// One editable member: its grip, place, identity, and the controls that
    /// change it, in fixed lanes so the rows line up.
    ///
    /// The caller passes the drag it is a part of — where the row would land and
    /// how far it has travelled — so the row stays a plain function of the
    /// draft's order plus the live drag.
    #[allow(clippy::too_many_arguments)]
    fn render_workspace_route_member_editor(
        &self,
        index: usize,
        member: &vibex_core::ModelRouteMember,
        count: usize,
        pending: bool,
        drop_after: Option<bool>,
        reorder_offset: Option<isize>,
        reorder_animation_id: Option<String>,
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
        let member_id = member.id.as_str().to_string();
        // The row is drawn at its dragged place while the draft still holds the
        // old order, so every control addresses the member by id rather than by
        // the position it happens to be painted at.
        let draft_index = self
            .route_group_model_members
            .iter()
            .position(|draft| draft.id == member.id)
            .unwrap_or(index);
        let drag_payload = RouteMemberOrderDrag {
            member_id: member_id.clone(),
            provider: profile.cloned(),
            provider_name: provider_name.clone(),
            model_id: member.model_id.clone(),
            enabled: member.enabled,
        };
        let drag_entity = cx.weak_entity();
        let drag_member_id = member_id.clone();
        let release_member_id = member_id.clone();
        let release_out_member_id = member_id.clone();
        let move_member_id = member_id.clone();
        let drag_handle = div()
            .id(SharedString::from(format!(
                "management-model-group-grip-{member_id}"
            )))
            .flex_none()
            .w(px(24.0))
            .h(px(MANAGEMENT_ROUTE_MEMBER_ROW_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .cursor_grab()
            .tooltip(move |window, cx| {
                Tooltip::new(management_locale_text(
                    "Drag to reorder members",
                    "拖动调整成员顺序",
                    "拖動調整成員順序",
                ))
                .build(window, cx)
            })
            .child(
                Icon::default()
                    .path("icons/vibex/grip-vertical.svg")
                    .size(px(16.0))
                    .text_color(cx.theme().muted_foreground.opacity(0.65)),
            )
            // A drop lands on a row, but a release outside every row still ends
            // on the preview the pointer left behind.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.finish_route_member_drag(&release_member_id, cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.finish_route_member_drag(&release_out_member_id, cx);
                }),
            )
            .on_drag(drag_payload, move |drag, cursor_offset, _, cx| {
                cx.stop_propagation();
                let _ = drag_entity.update(cx, |this, cx| {
                    this.start_route_member_drag(&drag_member_id, f32::from(cursor_offset.y), cx);
                });
                cx.new(|_| drag.clone())
            });
        let effort = member.reasoning_effort.clone().unwrap_or_default();
        let center = cx.weak_entity();
        let effort_menu = Button::new(SharedString::from(format!(
            "management-model-group-effort-{member_id}"
        )))
        .xsmall()
        .ghost()
        .label(route_group_effort_label(&effort))
        .tooltip(management_locale_text(
            "Reasoning effort for this member",
            "该成员的推理档位",
            "該成員的推理檔位",
        ))
        .accessibility_label(management_locale_text(
            "Reasoning effort for this member",
            "该成员的推理档位",
            "該成員的推理檔位",
        ))
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
                                this.set_route_group_model_effort(
                                    draft_index,
                                    value.to_string(),
                                    cx,
                                )
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
                Some(profile) => provider_logo_mark(profile, px(24.0), cx),
                None => ws_icon_mark(
                    "icons/vibex/database.svg",
                    px(24.0),
                    Some(cx.theme().warning),
                    cx,
                ),
            })
            .into_any_element();
        let member_weight = member.weight;
        let weight_center = cx.weak_entity();
        let weight_menu = Button::new(SharedString::from(format!(
            "management-model-group-weight-{member_id}"
        )))
        .xsmall()
        .ghost()
        .label(format!("×{}", member.weight))
        .tooltip(management_locale_text(
            "Weight for this member",
            "该成员的权重",
            "該成員的權重",
        ))
        .accessibility_label(management_locale_text(
            "Weight for this member",
            "该成员的权重",
            "該成員的權重",
        ))
        .dropdown_caret(true)
        .disabled(pending)
        .dropdown_menu(move |mut menu, _, _| {
            for weight in [1, 2, 3, 5] {
                let center = weight_center.clone();
                menu = menu.item(
                    PopupMenuItem::new(format!("×{weight}"))
                        .checked(member_weight == weight)
                        .on_click(move |_, _, cx| {
                            center
                                .update(cx, |this, cx| {
                                    this.set_route_group_model_weight(draft_index, weight, cx)
                                })
                                .ok();
                        }),
                );
            }
            menu
        });
        let trailing = h_flex()
            .items_center()
            .gap_2()
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(weight_menu)
                    .child(effort_menu)
                    .child(
                        Switch::new(SharedString::from(format!(
                            "management-model-group-enabled-{member_id}"
                        )))
                        .small()
                        .checked(member.enabled)
                        .tooltip(if member.enabled {
                            management_locale_text(
                                "Skip this member when routing",
                                "路由时跳过该成员",
                                "路由時跳過該成員",
                            )
                        } else {
                            management_locale_text(
                                "Use this member when routing",
                                "路由时使用该成员",
                                "路由時使用該成員",
                            )
                        })
                        .disabled(pending)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_route_group_model_enabled(draft_index, cx)
                        })),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_0p5()
                    .child(
                        Button::new(SharedString::from(format!(
                            "management-model-group-up-{member_id}"
                        )))
                        .xsmall()
                        .ghost()
                        .icon(IconName::ArrowUp)
                        .tooltip(management_locale_text("Move up", "上移", "上移"))
                        .accessibility_label(management_locale_text("Move up", "上移", "上移"))
                        .disabled(pending || draft_index == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.move_route_group_model_member(draft_index, true, cx)
                        })),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "management-model-group-down-{member_id}"
                        )))
                        .xsmall()
                        .ghost()
                        .icon(IconName::ArrowDown)
                        .tooltip(management_locale_text("Move down", "下移", "下移"))
                        .accessibility_label(management_locale_text("Move down", "下移", "下移"))
                        .disabled(pending || draft_index + 1 >= count)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.move_route_group_model_member(draft_index, false, cx)
                        })),
                    ),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "management-model-group-remove-{member_id}"
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
                    this.remove_route_group_model_member(draft_index, cx)
                })),
            )
            .into_any_element();
        let dragging = self
            .route_member_order_drag_state
            .as_ref()
            .is_some_and(|state| state.member_id == member_id);
        let row = h_flex()
            .id(SharedString::from(format!(
                "management-model-group-member-{member_id}"
            )))
            .relative()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_3()
            .py_2p5()
            .when(index > 0, |row| {
                row.border_t_1()
                    .border_color(cx.theme().border.opacity(0.6))
            })
            .when_some(drop_after, |row, after| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .h(px(2.0))
                        .bg(cx.theme().drag_border)
                        .map(|line| if after { line.bottom_0() } else { line.top_0() }),
                )
            })
            .when(dragging, |row| row.opacity(0.45))
            .on_drag_move(cx.listener(
                move |this, event: &DragMoveEvent<RouteMemberOrderDrag>, _, cx| {
                    let drag = event.drag(cx).clone();
                    let position = event.event.position;
                    let inside_target = position.x >= event.bounds.left() - px(24.0)
                        && position.x <= event.bounds.right() + px(24.0)
                        && position.y >= event.bounds.top() - px(2.0)
                        && position.y <= event.bounds.bottom() + px(2.0);
                    let next = (inside_target && drag.member_id != move_member_id).then_some(
                        RouteMemberOrderDropTarget {
                            member_id: move_member_id.clone(),
                            after: position.y >= event.bounds.center().y,
                        },
                    );
                    if let Some(next) = next {
                        this.preview_route_member_reorder(&drag, &next.member_id, next.after, cx);
                    }
                },
            ))
            .on_drop(
                cx.listener(move |this, drag: &RouteMemberOrderDrag, _, cx| {
                    this.finish_route_member_drag(&drag.member_id, cx);
                }),
            )
            .child(drag_handle)
            .child(mark)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .when(!member.enabled, |text| {
                                text.text_color(cx.theme().muted_foreground)
                            })
                            .child(provider_name),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(ws_mono_fact(member.model_id.clone(), cx)),
                    ),
            )
            .child(h_flex().flex_none().items_center().gap_2().child(trailing));
        if let (Some(offset), Some(animation_id)) = (reorder_offset, reorder_animation_id) {
            Transition::new(Duration::from_millis(
                MANAGEMENT_PROVIDER_REORDER_ANIMATION_MS,
            ))
            .ease(ease_out_cubic)
            .slide_y(
                px(offset as f32 * MANAGEMENT_ROUTE_MEMBER_ROW_HEIGHT),
                px(0.0),
            )
            .apply(row, animation_id)
            .into_any_element()
        } else {
            row.into_any_element()
        }
    }
}

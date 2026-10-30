//! One route group's page, the page that authors one, and the form both
//! share with the Agent dialog.
//!
//! A route group is one model name that tries several Provider models in an
//! order. Its page reads like a Provider's: an identity header with the
//! switch that turns it on, then tabs — Overview (who answers and who asks),
//! Members (the order, edited in place), Settings (how it picks, its limits,
//! whether pickers list it) and Traffic. The new-group page and the Agent
//! dialog draw the very same sections, so a setting found in one entry is
//! found, worded the same, in every other.

use super::*;

const ROUTE_TABS: [RouteGroupDetailTab; 4] = [
    RouteGroupDetailTab::Overview,
    RouteGroupDetailTab::Members,
    RouteGroupDetailTab::Settings,
    RouteGroupDetailTab::Traffic,
];

fn route_tab_label(tab: RouteGroupDetailTab) -> &'static str {
    match tab {
        RouteGroupDetailTab::Members => management_locale_text("Members", "成员", "成員"),
        RouteGroupDetailTab::Settings => management_locale_text("Settings", "设置", "設定"),
        RouteGroupDetailTab::Traffic => management_locale_text("Traffic", "流量", "流量"),
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

    // ----- Members & Settings, edited in place ------------------------------

    /// The Members tab: the attempt order itself, editable where it is read.
    fn render_workspace_route_members(&mut self, cx: &mut Context<Self>) -> AnyElement {
        ws_stack(vec![
            self.render_route_group_members_section(cx),
            self.render_route_group_form_footer(RouteGroupFormScope::Detail, cx),
        ])
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
            self.render_route_group_name_section(cx),
            self.render_route_group_members_section(cx),
            self.render_route_group_strategy_section(cx),
            self.render_route_group_limits_section(cx),
            self.render_route_group_visibility_section(cx),
            self.render_route_group_simulator_section(cx),
            self.render_route_group_form_footer(scope, cx),
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

    fn render_route_group_members_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
        let members = self.route_group_model_members.clone();
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
        ws_section(
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
        )
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
    fn render_route_group_form_footer(
        &mut self,
        scope: RouteGroupFormScope,
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
        ws_form_footer(
            (scope != RouteGroupFormScope::Page)
                .then(|| self.error.clone())
                .flatten(),
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
                    .on_click(cx.listener(|this, _, _, cx| this.save_route_group(cx)))
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
        let pending = self.mutation.is_some();
        let editing = self.route_group_editing_id.is_some();
        let back = Button::new("management-route-group-page-back")
            .small()
            .ghost()
            .icon(IconName::ArrowLeft)
            .label(management_locale_text("Back", "返回", "返回"))
            .disabled(pending)
            .on_click(cx.listener(|this, _, _, cx| this.close_route_group_editor(cx)))
            .into_any_element();
        let mark = ws_icon_tile(
            "icons/vibex/git-branch.svg",
            px(44.0),
            Some(cx.theme().primary),
            cx,
        );
        ws_authoring_page(
            "management-route-group-page-editor",
            back,
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

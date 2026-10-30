//! One shared Provider's page.
//!
//! Four tabs, each answering one question: Overview (is it working and who
//! uses it), Keys (what it signs with), Models (what it offers), Settings
//! (where and how it connects). The edit form lives on Settings itself, so
//! changing an address is never a separate mode to find.

use super::*;

/// The tabs, in the order the page draws them.
const PROVIDER_TABS: [ProviderDetailTab; 4] = [
    ProviderDetailTab::Overview,
    ProviderDetailTab::Keys,
    ProviderDetailTab::Models,
    ProviderDetailTab::Advanced,
];

/// A model row, and the settings that open under it, in the virtual list.
const MODEL_ROW_HEIGHT: f32 = 52.0;
const MODEL_EDITOR_HEIGHT: f32 = 430.0;

fn provider_tab_label(tab: ProviderDetailTab) -> &'static str {
    match tab {
        ProviderDetailTab::Keys => management_locale_text("Keys", "Key", "Key"),
        ProviderDetailTab::Models => management_locale_text("Models", "模型", "模型"),
        ProviderDetailTab::Advanced => management_locale_text("Settings", "设置", "設定"),
        _ => management_locale_text("Overview", "概览", "概覽"),
    }
}

impl ManagementCenter {
    pub(super) fn render_workspace_provider(
        &mut self,
        row: SharedProviderRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let profile = row.profile.clone();
        let tab = self.provider_detail_tab;
        let selected = PROVIDER_TABS
            .iter()
            .position(|item| *item == tab)
            .unwrap_or(0);
        let body = match tab {
            ProviderDetailTab::Keys => self.render_workspace_provider_keys(&profile, window, cx),
            ProviderDetailTab::Models => {
                self.render_workspace_provider_models(&profile, window, cx)
            }
            ProviderDetailTab::Advanced => {
                self.render_workspace_provider_settings(&row, window, cx)
            }
            _ => self.render_workspace_provider_overview(&row, cx),
        };
        v_flex()
            .id("management-provider-page")
            .w_full()
            .min_w_0()
            .gap_4()
            .child(self.render_workspace_provider_header(&row, cx))
            .child(ws_tabs(
                "management-provider-detail-tabs",
                PROVIDER_TABS
                    .iter()
                    .map(|tab| provider_tab_label(*tab))
                    .collect(),
                selected,
                |this, index, _, cx| {
                    if let Some(tab) = PROVIDER_TABS.get(index) {
                        this.provider_detail_tab = *tab;
                        cx.notify();
                    }
                },
                cx,
            ))
            .child(body)
            .into_any_element()
    }

    fn render_workspace_provider_header(
        &self,
        row: &SharedProviderRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let profile = &row.profile;
        let pending = self.mutation.is_some();
        let enabled = profile.status == vibex_core::ModelProviderProfileStatus::Enabled;
        let favorite = self.provider_favorites.contains(profile.id.as_str());
        let enabled_models = profile
            .configured_models
            .iter()
            .filter(|model| model.enabled)
            .count();
        let (health, health_status) = self.shared_provider_health_label(profile);
        let mut facts = Vec::new();
        if let Some(endpoint) = profile.primary_api_endpoint() {
            facts.push(ws_mono_fact(endpoint.url.clone(), cx));
        }
        facts.push(
            div()
                .child(format!(
                    "{enabled_models} {}",
                    management_locale_text("models", "个模型", "個模型")
                ))
                .into_any_element(),
        );
        facts.push(ws_state_fact(
            health,
            management_provider_health_color(health_status, cx),
        ));

        let mut badges = Vec::new();
        if !enabled {
            badges.push(gateway_provider_status_badge(profile, cx));
        }
        if profile.metadata.only_through_route_groups {
            badges.push(
                Tag::color(ColorName::Neutral)
                    .xsmall()
                    .child(management_locale_text(
                        "Route groups only",
                        "仅路由组",
                        "僅路由組",
                    ))
                    .into_any_element(),
            );
        }

        let testing = matches!(
            &self.mutation,
            Some(ManagementMutation::ProviderProbe(action)) if action.starts_with("test")
        );
        let center = cx.weak_entity();
        let menu_profile = profile.clone();
        let more = Button::new("management-shared-provider-more")
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
                let favorite_action = {
                    let center = center.clone();
                    let id = menu_profile.id.as_str().to_string();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| this.toggle_provider_favorite(id.clone(), cx))
                            .ok();
                    }
                };
                let refresh = {
                    let center = center.clone();
                    let id = menu_profile.id.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.refresh_shared_provider_models(id.clone(), cx)
                            })
                            .ok();
                    }
                };
                let delete = {
                    let center = center.clone();
                    let id = menu_profile.id.clone();
                    let label = menu_profile.display_name.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.confirm_delete_shared_provider(
                                    id.clone(),
                                    label.clone(),
                                    window,
                                    cx,
                                )
                            })
                            .ok();
                    }
                };
                menu.item(
                    PopupMenuItem::new(if favorite {
                        management_locale_text("Remove from favorites", "取消收藏", "取消收藏")
                    } else {
                        management_locale_text("Add to favorites", "收藏", "收藏")
                    })
                    .icon(if favorite {
                        IconName::Star
                    } else {
                        IconName::StarFill
                    })
                    .on_click(favorite_action),
                )
                .item(
                    PopupMenuItem::new(management_locale_text(
                        "Refresh models",
                        "刷新模型列表",
                        "重新整理模型清單",
                    ))
                    .icon(IconName::RefreshCw)
                    .on_click(refresh),
                )
                .separator()
                .item(
                    PopupMenuItem::new(management_locale_text(
                        "Delete Provider…",
                        "删除供应商…",
                        "刪除供應商…",
                    ))
                    .icon(Icon::default().path("icons/vibex/trash-2.svg"))
                    .on_click(delete),
                )
            });
        let actions = h_flex()
            .items_center()
            .gap_2()
            .child(
                Switch::new("management-shared-provider-enabled")
                    .small()
                    .checked(enabled)
                    .disabled(pending)
                    .label(management_locale_text("Enabled", "启用", "啟用"))
                    .on_click(cx.listener({
                        let id = profile.id.clone();
                        move |this, checked, _, cx| {
                            this.set_shared_provider_enabled(id.clone(), *checked, cx)
                        }
                    })),
            )
            .child(
                Button::new("management-shared-provider-test")
                    .small()
                    .outline()
                    .icon(Icon::default().path("icons/vibex/zap.svg"))
                    .label(management_locale_text("Test", "测试连接", "測試連線"))
                    .loading(testing)
                    .disabled(pending)
                    .on_click(cx.listener({
                        let id = profile.id.clone();
                        move |this, _, _, cx| this.test_shared_provider(id.clone(), cx)
                    })),
            )
            .child(more)
            .into_any_element();
        ws_page_header(
            provider_logo_tile(profile, px(48.0), cx),
            ws_title(profile.display_name.clone(), badges),
            ws_facts(facts, cx),
            actions,
        )
    }

    // ----- Overview --------------------------------------------------------

    fn render_workspace_provider_overview(
        &mut self,
        row: &SharedProviderRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let profile = &row.profile;
        let enabled_models = profile
            .configured_models
            .iter()
            .filter(|model| model.enabled)
            .count();
        let ready_keys = profile
            .credentials
            .iter()
            .filter(|credential| credential.status == vibex_core::AgentCredentialStatus::Ready)
            .count();
        let (health, health_status) = self.shared_provider_health_label(profile);
        let tiles = ws_tile_row(vec![
            management_stat_tile(
                management_locale_text("Models", "模型", "模型"),
                enabled_models.to_string(),
                if profile.configured_models.len() > enabled_models {
                    format!(
                        "{} {}",
                        profile.configured_models.len() - enabled_models,
                        management_locale_text("turned off", "个已停用", "個已停用")
                    )
                } else {
                    management_locale_text("All turned on", "全部启用", "全部啟用").to_string()
                },
                None,
                cx,
            ),
            management_stat_tile(
                management_locale_text("Keys", "Key", "Key"),
                format!("{ready_keys}/{}", profile.credentials.len()),
                if profile.credentials.is_empty() {
                    management_locale_text("Add one to use it", "添加后才能使用", "新增後才能使用")
                        .to_string()
                } else if ready_keys == profile.credentials.len() {
                    management_locale_text("All ready", "全部可用", "全部可用").to_string()
                } else {
                    management_locale_text("Some unavailable", "部分不可用", "部分無法使用")
                        .to_string()
                },
                (ready_keys < profile.credentials.len() || profile.credentials.is_empty())
                    .then(|| cx.theme().warning),
                cx,
            ),
            management_stat_tile(
                management_locale_text("Health", "健康", "健康"),
                health.to_string(),
                management_locale_text("Latest probe", "最近一次探测", "最近一次探測").to_string(),
                Some(management_provider_health_color(health_status, cx)),
                cx,
            ),
            management_stat_tile(
                "Agent",
                row.agents.len().to_string(),
                management_locale_text("Using this Provider", "正在使用", "正在使用").to_string(),
                None,
                cx,
            ),
        ]);

        // Agents.
        let agents = self.workspace_provider_agent_rows(row);
        let profile_id = profile.id.clone();
        let legacy = profile.legacy_provider_profile_id.clone();
        let set_default: Rc<dyn Fn(&mut ManagementCenter, String, &mut Context<ManagementCenter>)> =
            Rc::new({
                let profile_id = profile_id.clone();
                move |this, agent_id, cx| {
                    if let Some(legacy) = this.workspace_provider_legacy_id(&profile_id, &agent_id)
                    {
                        this.set_agent_default_model_source(
                            agent_id,
                            vibex_core::AgentModelSourceRef::Provider(legacy),
                            cx,
                        );
                    }
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
        > = Rc::new({
            let profile_id = profile_id.clone();
            move |this, agent_id, bind, window, cx| {
                if bind {
                    let Ok(agent) = AgentId::parse(agent_id) else {
                        return;
                    };
                    this.bind_provider_to_agent(
                        agent,
                        legacy
                            .clone()
                            .unwrap_or_else(vibex_core::ProviderProfileId::new),
                        Some(profile_id.clone()),
                        cx,
                    );
                } else if let Some(legacy) =
                    this.workspace_provider_legacy_id(&profile_id, &agent_id)
                {
                    let label = this.workspace_agent_label(&agent_id);
                    let Ok(agent) = AgentId::parse(agent_id) else {
                        return;
                    };
                    this.try_unbind_model_source(
                        agent,
                        vibex_core::AgentModelSourceRef::Provider(legacy.clone()),
                        UnbindModelSource::Provider(legacy),
                        label,
                        window,
                        cx,
                    );
                }
            }
        });
        let agent_list = self.render_workspace_agent_list(
            "management-provider",
            agents,
            set_default,
            toggle,
            cx,
        );

        // Route groups this Provider serves.
        let memberships = self
            .model_routes
            .iter()
            .filter_map(|route| {
                route
                    .members
                    .iter()
                    .position(|member| member.model_provider_profile_id == profile.id)
                    .map(|position| (route.clone(), position))
            })
            .collect::<Vec<_>>();
        let mut groups = v_flex().w_full().min_w_0();
        if memberships.is_empty() {
            groups = groups.child(ws_empty_line(
                management_locale_text(
                    "No route group uses it. Add it to a group to take part in failover.",
                    "没有路由组使用它。加入路由组后才会参与故障转移。",
                    "沒有路由組使用它。加入路由組後才會參與容錯移轉。",
                ),
                cx,
            ));
        }
        for (index, (route, position)) in memberships.iter().enumerate() {
            let select_id = route.id.as_str().to_string();
            groups = groups.child(ws_row(
                SharedString::from(format!("management-provider-route-{}", route.id.as_str())),
                ws_icon_tile("icons/vibex/git-branch.svg", px(30.0), None, cx),
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(route.display_name.clone())
                    .into_any_element(),
                Some(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{} {} · {}",
                            management_locale_text("Priority", "优先级", "優先級"),
                            position + 1,
                            management_route_strategy_label(route.strategy)
                        ))
                        .into_any_element(),
                ),
                Button::new(SharedString::from(format!(
                    "management-provider-route-open-{}",
                    route.id.as_str()
                )))
                .small()
                .ghost()
                .label(management_locale_text("Open", "打开", "開啟"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_workspace_route_group(select_id.clone(), cx)
                }))
                .into_any_element(),
                index == 0,
                cx,
            ));
        }

        // Traffic.
        let cutoff = vibex_core::unix_timestamp_ms() - self.traffic_range.window_ms();
        let traffic = WorkspaceTraffic::collect(self.gateway_requests.iter().filter(|request| {
            request.model_provider_profile_id == profile.id && request.created_at_ms >= cutoff
        }));

        ws_stack(vec![
            tiles,
            ws_section(
                management_locale_text("Agents", "Agent", "Agent"),
                Some(management_locale_text(
                    "Turn an Agent on to let it use this Provider's models.",
                    "打开开关即可让 Agent 使用该供应商的模型。",
                    "打開開關即可讓 Agent 使用該供應商的模型。",
                )),
                None,
                agent_list,
                cx,
            ),
            ws_section(
                management_locale_text("Route groups", "路由组", "路由組"),
                Some(management_locale_text(
                    "Groups that try this Provider, and where it stands in their order.",
                    "会调用该供应商的路由组，以及它在其中的顺位。",
                    "會呼叫該供應商的路由組，以及它在其中的順位。",
                )),
                None,
                groups.into_any_element(),
                cx,
            ),
            ws_section(
                management_locale_text("Traffic", "流量", "流量"),
                Some(management_locale_text(
                    "Requests the gateway sent here. No cost or quota is tracked.",
                    "经网关发往这里的请求；不统计费用或额度。",
                    "經閘道發往這裡的請求；不統計費用或額度。",
                )),
                Some(self.workspace_range_menu("management-provider-traffic-range", cx)),
                traffic.tiles(cx),
                cx,
            ),
        ])
    }

    fn workspace_provider_legacy_id(
        &self,
        profile_id: &vibex_core::ModelProviderProfileId,
        agent_id: &str,
    ) -> Option<vibex_core::ProviderProfileId> {
        self.provider_bindings
            .iter()
            .find(|binding| {
                binding.agent_id.as_str() == agent_id
                    && binding.model_provider_profile_id.as_ref() == Some(profile_id)
            })
            .and_then(|binding| binding.legacy_provider_profile_id.clone())
            .or_else(|| {
                self.pending_provider_bindings
                    .get(&(profile_id.as_str().to_string(), agent_id.to_string()))
                    .cloned()
            })
    }

    fn workspace_provider_agent_rows(&self, row: &SharedProviderRow) -> Vec<WorkspaceAgentRow> {
        let mut rows = Vec::new();
        for agent_id in &row.agents {
            let is_default = self
                .workspace_provider_legacy_id(&row.profile.id, agent_id)
                .is_some_and(|legacy| {
                    self.resolved_agent_default_source(agent_id).as_ref()
                        == Some(&vibex_core::AgentModelSourceRef::Provider(legacy))
                });
            rows.push(WorkspaceAgentRow {
                agent_id: agent_id.clone(),
                label: self.workspace_agent_label(agent_id),
                bound: true,
                is_default,
            });
        }
        for (agent_id, label) in self.workspace_configurable_agents() {
            if !row.agents.contains(&agent_id) {
                rows.push(WorkspaceAgentRow {
                    agent_id,
                    label,
                    bound: false,
                    is_default: false,
                });
            }
        }
        rows
    }

    // ----- Keys ------------------------------------------------------------

    fn render_workspace_provider_keys(
        &mut self,
        profile: &vibex_core::ModelProviderProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pending = self.mutation.is_some();
        let count = profile.credentials.len();
        let mut list = v_flex().w_full().min_w_0();
        for (index, credential) in profile.credentials.iter().enumerate() {
            let summary = self
                .gateway
                .upstreams
                .iter()
                .flat_map(|upstream| upstream.keys.iter())
                .find(|key| key.credential_id == credential.id)
                .cloned();
            let key_enabled = summary
                .as_ref()
                .is_none_or(|key| key.state != vibex_core::GatewayKeyState::Disabled);
            let preferred = self
                .gateway
                .settings
                .policy
                .preferred_keys
                .get(&profile.id)
                .is_some_and(|id| id == &credential.id);
            let testing = matches!(
                &self.mutation,
                Some(ManagementMutation::ProviderProbe(action))
                    if action == &format!("key-test:{}", credential.id.as_str())
            );
            let state = match credential.status {
                vibex_core::AgentCredentialStatus::Ready => None,
                vibex_core::AgentCredentialStatus::Missing => Some(
                    Tag::color(ColorName::Amber)
                        .xsmall()
                        .child(management_locale_text("Missing", "缺失", "缺失")),
                ),
                vibex_core::AgentCredentialStatus::Expired => Some(
                    Tag::color(ColorName::Red)
                        .xsmall()
                        .child(management_locale_text("Expired", "已过期", "已過期")),
                ),
                _ => Some(
                    Tag::color(ColorName::Neutral)
                        .xsmall()
                        .child(management_locale_text("Not ready", "未就绪", "未就緒")),
                ),
            };
            let replace_open = self
                .gateway_key_replace_target
                .as_ref()
                .is_some_and(|(target, id)| target == &profile.id && id == &credential.id);
            let center = cx.weak_entity();
            let menu_profile = profile.id.clone();
            let menu_credential = credential.id.clone();
            let more = Button::new(SharedString::from(format!(
                "management-shared-provider-key-more-{}",
                credential.id.as_str()
            )))
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
                let prefer = {
                    let center = center.clone();
                    let profile_id = menu_profile.clone();
                    let credential_id = menu_credential.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.update_gateway_settings(
                                    |request| {
                                        request.account_updates.push(
                                            vibex_core::GatewayAccountUpdate {
                                                model_provider_profile_id: profile_id.clone(),
                                                credential_id: credential_id.clone(),
                                                enabled: None,
                                                preferred: Some(true),
                                            },
                                        );
                                    },
                                    cx,
                                )
                            })
                            .ok();
                    }
                };
                let replace = {
                    let center = center.clone();
                    let profile_id = menu_profile.clone();
                    let credential_id = menu_credential.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.gateway_key_replace_target = (!replace_open)
                                    .then(|| (profile_id.clone(), credential_id.clone()));
                                this.gateway_key_replace_input = None;
                                this.gateway_key_replace_error = None;
                                cx.notify();
                            })
                            .ok();
                    }
                };
                let scope = {
                    let center = center.clone();
                    let profile_id = menu_profile.clone();
                    let credential_id = menu_credential.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.open_gateway_key_policy_editor(
                                    &profile_id,
                                    &credential_id,
                                    window,
                                    cx,
                                )
                            })
                            .ok();
                    }
                };
                let remove = {
                    let center = center.clone();
                    let profile_id = menu_profile.clone();
                    let credential_id = menu_credential.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.confirm_remove_provider_key(
                                    profile_id.clone(),
                                    credential_id.clone(),
                                    count,
                                    window,
                                    cx,
                                )
                            })
                            .ok();
                    }
                };
                menu.item(
                    PopupMenuItem::new(management_locale_text(
                        "Prefer this Key",
                        "优先使用该 Key",
                        "優先使用該 Key",
                    ))
                    .checked(preferred)
                    .on_click(prefer),
                )
                .item(
                    PopupMenuItem::new(management_locale_text(
                        "Replace Key…",
                        "更换 Key…",
                        "更換 Key…",
                    ))
                    .on_click(replace),
                )
                .item(
                    PopupMenuItem::new(management_locale_text(
                        "Scope and proxy…",
                        "范围与代理…",
                        "範圍與代理…",
                    ))
                    .on_click(scope),
                )
                .separator()
                .item(
                    PopupMenuItem::new(management_locale_text(
                        "Remove Key…",
                        "移除 Key…",
                        "移除 Key…",
                    ))
                    .icon(Icon::default().path("icons/vibex/trash-2.svg"))
                    .on_click(remove),
                )
            });
            let trailing = h_flex()
                .items_center()
                .gap_2()
                .children(state)
                .when(preferred, |trailing| {
                    trailing.child(
                        Tag::color(ColorName::Blue)
                            .xsmall()
                            .child(management_locale_text("Preferred", "优先", "優先")),
                    )
                })
                .child(
                    Button::new(SharedString::from(format!(
                        "management-shared-provider-test-key-{}",
                        credential.id.as_str()
                    )))
                    .small()
                    .ghost()
                    .label(management_locale_text("Test", "测试", "測試"))
                    .loading(testing)
                    .disabled(pending)
                    .on_click(cx.listener({
                        let profile_id = profile.id.clone();
                        let credential_id = credential.id.clone();
                        move |this, _, _, cx| {
                            this.test_shared_provider_key(
                                profile_id.clone(),
                                credential_id.clone(),
                                cx,
                            )
                        }
                    })),
                )
                .child(
                    Switch::new(SharedString::from(format!(
                        "management-shared-provider-toggle-key-{}",
                        credential.id.as_str()
                    )))
                    .small()
                    .checked(key_enabled)
                    .disabled(pending)
                    .on_click(cx.listener({
                        let profile_id = profile.id.clone();
                        let credential_id = credential.id.clone();
                        move |this, checked, _, cx| {
                            this.set_gateway_key_enabled(
                                profile_id.clone(),
                                credential_id.clone(),
                                *checked,
                                cx,
                            )
                        }
                    })),
                )
                .child(more)
                .into_any_element();
            let usage = summary.as_ref().map(|key| {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} {}",
                        gateway_token_count(key.used_tokens),
                        management_locale_text("tokens used", "Token 已用", "Token 已用")
                    ))
                    .into_any_element()
            });
            list = list.child(ws_row(
                SharedString::from(format!(
                    "management-provider-key-{}",
                    credential.id.as_str()
                )),
                ws_icon_tile("icons/vibex/file-key.svg", px(30.0), None, cx),
                self.render_provider_key_value(
                    &profile.id,
                    &credential.id,
                    &credential.masked_hint,
                    &credential.display_name,
                    credential.credential.secret_reference().is_some(),
                    cx,
                ),
                usage,
                trailing,
                index == 0,
                cx,
            ));
            list = list
                .child(self.render_gateway_key_replace(&profile.id, &credential.id, window, cx))
                .child(self.render_gateway_key_policy(profile, credential, window, cx));
            if let Some(key) = summary.as_ref() {
                list = list.child(self.render_gateway_key_limits(key, window, cx));
            }
        }
        if profile.credentials.is_empty() {
            list = list.child(ws_empty_line(
                management_locale_text(
                    "No Key yet. Every Agent that uses this Provider signs with the Keys added here.",
                    "还没有 Key。使用该供应商的每个 Agent 都会用这里添加的 Key 签名。",
                    "還沒有 Key。使用該供應商的每個 Agent 都會用這裡新增的 Key 簽名。",
                ),
                cx,
            ));
        }
        let adding = self.gateway_key_add_target.as_deref() == Some(profile.id.as_str())
            || self.gateway_key_batch_target.as_deref() == Some(profile.id.as_str());
        let add = self.render_gateway_add_key(&profile.id, window, cx);
        let batch = self.render_gateway_batch_add_key(&profile.id, window, cx);
        let footer = if adding {
            v_flex()
                .w_full()
                .gap_2()
                .pt_3()
                .child(add)
                .child(batch)
                .into_any_element()
        } else {
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .pt_3()
                .child(add)
                .child(batch)
                .into_any_element()
        };
        ws_stack(vec![ws_section(
            management_locale_text("Keys", "Key", "Key"),
            Some(management_locale_text(
                "Shared by every Agent that uses this Provider. The gateway rotates between the ones turned on.",
                "使用该供应商的所有 Agent 共用；网关会在已启用的 Key 之间轮换。",
                "使用該供應商的所有 Agent 共用；閘道會在已啟用的 Key 之間輪換。",
            )),
            None,
            v_flex()
                .w_full()
                .child(list)
                .child(footer)
                .into_any_element(),
            cx,
        )])
    }

    // ----- Models ----------------------------------------------------------

    fn render_workspace_provider_models(
        &mut self,
        profile: &vibex_core::ModelProviderProfile,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self
            .provider_models_search
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let active_filter = self.provider_models_filter;
        let filter_center = cx.weak_entity();
        let filter = Button::new("management-provider-models-filter")
            .small()
            .outline()
            .label(active_filter.label())
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for filter in ProviderModelsFilter::ALL {
                    let center = filter_center.clone();
                    menu = menu.item(
                        PopupMenuItem::new(filter.label())
                            .checked(filter == active_filter)
                            .on_click(move |_, _, cx| {
                                center
                                    .update(cx, |this, cx| {
                                        this.provider_models_filter = filter;
                                        cx.notify();
                                    })
                                    .ok();
                            }),
                    );
                }
                menu
            });
        let bulk_center = cx.weak_entity();
        let bulk_profile = profile.id.clone();
        let bulk = Button::new("management-provider-models-bulk")
            .small()
            .ghost()
            .icon(IconName::Ellipsis)
            .tooltip(management_locale_text(
                "More actions",
                "更多操作",
                "更多操作",
            ))
            .disabled(self.mutation.is_some() || profile.configured_models.is_empty())
            .dropdown_menu(move |menu, _, _| {
                let set_all = |enabled: bool| {
                    let center = bulk_center.clone();
                    let id = bulk_profile.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.set_shared_provider_models_enabled(id.clone(), enabled, cx)
                            })
                            .ok();
                    }
                };
                let refresh = {
                    let center = bulk_center.clone();
                    let id = bulk_profile.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.refresh_shared_provider_models(id.clone(), cx)
                            })
                            .ok();
                    }
                };
                menu.item(
                    PopupMenuItem::new(management_locale_text(
                        "Refresh from the Provider",
                        "从供应商刷新",
                        "從供應商重新整理",
                    ))
                    .icon(IconName::RefreshCw)
                    .on_click(refresh),
                )
                .separator()
                .item(
                    PopupMenuItem::new(management_locale_text(
                        "Turn on all models",
                        "全部启用",
                        "全部啟用",
                    ))
                    .on_click(set_all(true)),
                )
                .item(
                    PopupMenuItem::new(management_locale_text(
                        "Turn off all models",
                        "全部停用",
                        "全部停用",
                    ))
                    .on_click(set_all(false)),
                )
            });
        // Fetching is the first thing a Provider with no models needs, so it
        // is a visible command, as it is in the Agent dialog, not a menu item.
        let fetching = matches!(
            &self.mutation,
            Some(ManagementMutation::ProviderProbe(action))
                if action == &format!("shared-models:{}", profile.id.as_str())
        );
        let fetch = Button::new("management-provider-models-fetch")
            .small()
            .outline()
            .icon(Icon::default().path("icons/vibex/rotate-ccw.svg"))
            .label(management_fetch_models_label())
            .loading(fetching)
            .disabled(self.mutation.is_some())
            .on_click(cx.listener({
                let id = profile.id.clone();
                move |this, _, _, cx| this.refresh_shared_provider_models(id.clone(), cx)
            }));
        let toolbar = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(management_search_input_compact(
                        &self.provider_models_search,
                        cx,
                    )),
            )
            .child(filter)
            .child(fetch)
            .child(bulk);
        // A model the endpoint does not list is added by id, the same
        // "Custom model" row the Agent dialog's picker ends with.
        let add_row = h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .pt_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.provider_model_add_input).small().w_full()),
            )
            .child(
                Button::new("management-provider-model-add")
                    .small()
                    .secondary()
                    .icon(IconName::Plus)
                    .label(management_add_label())
                    .disabled(self.mutation.is_some())
                    .on_click(cx.listener({
                        let id = profile.id.clone();
                        move |this, _, window, cx| {
                            this.add_shared_provider_model(id.clone(), window, cx)
                        }
                    })),
            );
        let matched = profile
            .configured_models
            .iter()
            .filter(|model| query.is_empty() || model.id.to_lowercase().contains(&query))
            .filter(|model| match active_filter {
                ProviderModelsFilter::All => true,
                ProviderModelsFilter::Enabled => model.enabled,
                ProviderModelsFilter::Disabled => !model.enabled,
                ProviderModelsFilter::Reasoning => model.capabilities.reasoning == Some(true),
                ProviderModelsFilter::Vision => model.capabilities.image_input == Some(true),
            })
            .cloned()
            .collect::<Vec<_>>();
        let enabled = profile
            .configured_models
            .iter()
            .filter(|model| model.enabled)
            .count();
        let content = if matched.is_empty() {
            ws_empty_line(
                if profile.configured_models.is_empty() {
                    management_locale_text(
                        "No models yet. Fetch the Provider's list, or add a model id below.",
                        "还没有模型。可以拉取供应商的模型列表，也可以在下方直接添加模型 ID。",
                        "還沒有模型。可以擷取供應商的模型清單，也可以在下方直接新增模型 ID。",
                    )
                } else {
                    management_locale_text(
                        "No model matches. Clear the search or the filter.",
                        "没有匹配的模型，请清除搜索或筛选。",
                        "沒有符合的模型，請清除搜尋或篩選。",
                    )
                },
                cx,
            )
        } else {
            let profile_rc = Rc::new(profile.clone());
            let models_rc = Rc::new(matched);
            let expanded = self.provider_model_edit_id.clone();
            let sizes = Rc::new(
                models_rc
                    .iter()
                    .map(|model| {
                        let height = if expanded.as_deref() == Some(model.id.as_str()) {
                            MODEL_ROW_HEIGHT + MODEL_EDITOR_HEIGHT
                        } else {
                            MODEL_ROW_HEIGHT
                        };
                        gpui_size(px(0.0), px(height))
                    })
                    .collect::<Vec<GpuiSize<gpui::Pixels>>>(),
            );
            let height = (models_rc.len() as f32 * MODEL_ROW_HEIGHT).clamp(160.0, 560.0)
                + if expanded.is_some() {
                    MODEL_EDITOR_HEIGHT
                } else {
                    0.0
                };
            div()
                .w_full()
                .h(px(height))
                .min_h_0()
                .child(v_virtual_list(
                    cx.entity(),
                    SharedString::from(format!("management-provider-models-{}", profile.id)),
                    sizes,
                    move |this, range: Range<usize>, window, cx| {
                        range
                            .filter_map(|index| {
                                let model = models_rc.get(index)?;
                                Some(this.render_shared_provider_model_row(
                                    &profile_rc,
                                    model,
                                    window,
                                    cx,
                                ))
                            })
                            .collect()
                    },
                ))
                .into_any_element()
        };
        ws_stack(vec![ws_section(
            management_locale_text("Models", "模型", "模型"),
            Some(management_locale_text(
                "What this Provider offers to the Agents that use it. Turned-off models stay out of every picker.",
                "该供应商提供给 Agent 的模型；停用的模型不会出现在任何选择器中。",
                "該供應商提供給 Agent 的模型；停用的模型不會出現在任何選擇器中。",
            )),
            Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{enabled}/{} {}",
                        profile.configured_models.len(),
                        management_locale_text("on", "已启用", "已啟用")
                    ))
                    .into_any_element(),
            ),
            v_flex()
                .w_full()
                .gap_3()
                .child(toolbar)
                .child(content)
                .child(add_row)
                .into_any_element(),
            cx,
        )])
    }

    // ----- Settings --------------------------------------------------------

    fn render_workspace_provider_settings(
        &mut self,
        row: &SharedProviderRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let profile = row.profile.clone();
        // The form always describes the record on screen: it loads on first
        // view and reloads when the record changes underneath it.
        if self.shared_provider_edit_target.as_ref() != Some(&profile.id)
            || self.workspace_edit_revision != Some(profile.revision)
        {
            self.open_shared_provider_editor(&profile, window, cx);
            self.workspace_edit_revision = Some(profile.revision);
        }
        let pending = self.mutation.is_some();
        let category = self.shared_provider_edit_category.clone();

        let identity = v_flex()
            .w_full()
            .gap_4()
            .child(ws_field(
                management_locale_text("Name", "名称", "名稱"),
                None,
                ws_input(
                    &self.shared_provider_edit_name,
                    false,
                    self.shared_provider_edit_name_error.as_deref(),
                    cx,
                ),
                cx,
            ))
            .child(ws_field(
                management_locale_text("Kind", "类型", "類型"),
                None,
                ws_choice_cards(
                    "management-shared-provider-category",
                    vec![
                        (
                            "vendor",
                            SharedString::from(management_locale_text("Official", "官方", "官方")),
                            Some(SharedString::from(management_locale_text(
                                "The model vendor's own API",
                                "模型厂商自己的 API",
                                "模型廠商自己的 API",
                            ))),
                        ),
                        (
                            "relay",
                            SharedString::from(management_locale_text("Relay", "中转", "中轉")),
                            Some(SharedString::from(management_locale_text(
                                "A service that resells other vendors' models",
                                "转售其他厂商模型的服务",
                                "轉售其他廠商模型的服務",
                            ))),
                        ),
                        (
                            "local",
                            SharedString::from(management_locale_text("Local", "本机", "本機")),
                            Some(SharedString::from(management_locale_text(
                                "A server on this machine or network",
                                "运行在本机或局域网的服务",
                                "執行在本機或區域網路的服務",
                            ))),
                        ),
                    ],
                    category.as_deref(),
                    pending,
                    |this, value, cx| {
                        this.shared_provider_edit_category = Some(value.to_string());
                        cx.notify();
                    },
                    cx,
                ),
                cx,
            ));

        let connection = v_flex()
            .w_full()
            .gap_4()
            .child(ws_field(
                management_locale_text("API address", "API 地址", "API 位址"),
                Some(management_locale_text(
                    "The base URL requests are sent to.",
                    "请求发往的基础地址。",
                    "請求發往的基礎位址。",
                )),
                ws_input(
                    &self.shared_provider_edit_base_url,
                    false,
                    self.shared_provider_edit_base_url_error.as_deref(),
                    cx,
                ),
                cx,
            ))
            .child(ws_field_pair(
                ws_field(
                    management_locale_text("Model list address", "模型列表地址", "模型清單位址"),
                    Some(management_locale_text(
                        "Leave empty to use the API address.",
                        "留空则使用 API 地址。",
                        "留空則使用 API 位址。",
                    )),
                    ws_input(&self.shared_provider_edit_models_url, false, None, cx),
                    cx,
                ),
                ws_field(
                    management_locale_text("Sign-in address", "认证地址", "認證位址"),
                    None,
                    ws_input(&self.shared_provider_edit_auth_url, false, None, cx),
                    cx,
                ),
            ))
            .child(ws_field_pair(
                ws_field(
                    management_locale_text("Proxy", "代理", "代理"),
                    Some(if self.shared_provider_edit_proxy_disabled {
                        management_locale_text(
                            "Direct connection: the system proxy is ignored.",
                            "直连：忽略系统代理。",
                            "直連：忽略系統代理。",
                        )
                    } else {
                        management_locale_text(
                            "Empty follows the system proxy.",
                            "留空则跟随系统代理。",
                            "留空則跟隨系統代理。",
                        )
                    }),
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .when(self.shared_provider_edit_proxy_disabled, |field| {
                                    field.opacity(0.5)
                                })
                                .child(ws_input(
                                    &self.shared_provider_edit_proxy_url,
                                    false,
                                    None,
                                    cx,
                                )),
                        )
                        .child(
                            Switch::new("management-shared-provider-proxy-disabled")
                                .small()
                                .label(management_locale_text("Direct", "直连", "直連"))
                                .checked(self.shared_provider_edit_proxy_disabled)
                                .disabled(pending)
                                .on_click(cx.listener(|this, checked, _, cx| {
                                    this.shared_provider_edit_proxy_disabled = *checked;
                                    cx.notify();
                                })),
                        )
                        .into_any_element(),
                    cx,
                ),
                ws_field(
                    management_locale_text("Proxy service", "代理服务地址", "代理服務位址"),
                    Some(management_locale_text(
                        "A relay endpoint that forwards for this Provider.",
                        "为该供应商转发请求的服务地址。",
                        "為該供應商轉發請求的服務位址。",
                    )),
                    ws_input(
                        &self.shared_provider_edit_proxy_endpoint_url,
                        false,
                        None,
                        cx,
                    ),
                    cx,
                ),
            ));

        let request = v_flex()
            .w_full()
            .gap_4()
            .child(ws_field(
                management_locale_text("Request headers", "请求头", "請求標頭"),
                Some(management_locale_text(
                    "A JSON object of header names and values. Use {\"variable\": \"session_id\"} for the session header. Secret headers are kept but not shown.",
                    "JSON 对象，键为请求头名称。会话请求头写作 {\"variable\": \"session_id\"}。Secret 请求头会保留但不显示。",
                    "JSON 物件，鍵為請求標頭名稱。工作階段請求標頭寫作 {\"variable\": \"session_id\"}。Secret 請求標頭會保留但不顯示。",
                )),
                Textarea::new(&self.shared_provider_edit_headers)
                    .small()
                    .w_full()
                    .h(px(96.0))
                    .into_any_element(),
                cx,
            ))
            .child(ws_field_pair(
                ws_field(
                    management_locale_text("models.dev id", "models.dev ID", "models.dev ID"),
                    Some(management_locale_text(
                        "Only powers suggestions; never changes a request.",
                        "仅用于提供建议，不会改变请求。",
                        "僅用於提供建議，不會改變請求。",
                    )),
                    ws_input(&self.shared_provider_edit_catalog_id, false, None, cx),
                    cx,
                ),
                ws_field(
                    management_locale_text("Icon", "图标", "圖示"),
                    Some(management_locale_text(
                        "A bundled icon key, such as openai.",
                        "内置图标的 key，例如 openai。",
                        "內建圖示的 key，例如 openai。",
                    )),
                    ws_input(&self.shared_provider_edit_icon, false, None, cx),
                    cx,
                ),
            ));

        let profile_id = profile.id.clone();
        let form = ws_section(
            management_locale_text("Connection", "连接", "連線"),
            Some(management_locale_text(
                "Applies to every Agent that uses this Provider once saved.",
                "保存后对所有使用该供应商的 Agent 生效。",
                "儲存後對所有使用該供應商的 Agent 生效。",
            )),
            None,
            v_flex()
                .w_full()
                .gap_5()
                .child(identity)
                .child(Separator::horizontal())
                .child(connection)
                .child(Separator::horizontal())
                .child(request)
                .child(ws_form_footer(
                    self.shared_provider_edit_error.clone(),
                    vec![
                        Button::new("management-shared-provider-edit-cancel")
                            .small()
                            .ghost()
                            .label(management_locale_text(
                                "Discard changes",
                                "放弃修改",
                                "放棄修改",
                            ))
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.workspace_edit_revision = None;
                                cx.notify();
                            }))
                            .into_any_element(),
                        Button::new("management-shared-provider-edit-save")
                            .small()
                            .primary()
                            .label(management_locale_text("Save", "保存", "儲存"))
                            .loading(matches!(
                                self.mutation,
                                Some(ManagementMutation::ProfileUpdate(_))
                            ))
                            .disabled(pending)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.save_shared_provider_editor(profile_id.clone(), cx)
                            }))
                            .into_any_element(),
                    ],
                    cx,
                ))
                .into_any_element(),
            cx,
        );

        ws_stack(vec![
            form,
            self.render_workspace_provider_behaviour(&profile, cx),
            self.render_shared_provider_endpoint_probes(&profile, cx),
            self.render_shared_provider_advanced_editor(&profile, window, cx),
            self.render_workspace_provider_danger(row, cx),
        ])
    }

    /// Settings that take effect the moment they change.
    fn render_workspace_provider_behaviour(
        &self,
        profile: &vibex_core::ModelProviderProfile,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pending = self.mutation.is_some();
        let auth_modes: [(&'static str, &'static str); 4] = [
            ("auto", management_locale_text("Auto", "自动", "自動")),
            ("x-api-key", "x-api-key"),
            ("bearer", "Authorization: Bearer"),
            (
                "both",
                management_locale_text("Both", "两者都发", "兩者都發"),
            ),
        ];
        let ua_modes: [(&'static str, &'static str); 2] = [
            (
                "default",
                management_locale_text("Vibex default", "Vibex 默认", "Vibex 預設"),
            ),
            (
                "pass_through",
                management_locale_text(
                    "Pass the Agent's through",
                    "透传 Agent 的",
                    "透傳 Agent 的",
                ),
            ),
        ];
        let auth = management_option_menu(
            "management-shared-provider-auth-mode",
            &auth_modes,
            profile
                .metadata
                .auth_header_mode
                .as_deref()
                .unwrap_or("auto"),
            pending,
            cx.weak_entity(),
            {
                let id = profile.id.clone();
                move |this, value, cx| {
                    this.update_shared_provider_metadata(
                        id.clone(),
                        move |metadata| {
                            metadata.auth_header_mode = (value != "auto").then(|| value.to_string())
                        },
                        cx,
                    )
                }
            },
        );
        let ua = management_option_menu(
            "management-shared-provider-ua-mode",
            &ua_modes,
            profile
                .metadata
                .user_agent_mode
                .as_deref()
                .unwrap_or("default"),
            pending,
            cx.weak_entity(),
            {
                let id = profile.id.clone();
                move |this, value, cx| {
                    this.update_shared_provider_metadata(
                        id.clone(),
                        move |metadata| {
                            metadata.user_agent_mode =
                                (value != "default").then(|| value.to_string())
                        },
                        cx,
                    )
                }
            },
        );
        let tls_locked = profile.metadata.catalog_id.is_some()
            && profile.metadata.category.as_deref() != Some("local");
        let switch = |id: &'static str,
                      checked: bool,
                      disabled: bool,
                      update: fn(&mut vibex_core::ModelProviderMetadata, bool),
                      cx: &mut Context<Self>| {
            let profile_id = profile.id.clone();
            Switch::new(id)
                .small()
                .checked(checked)
                .disabled(disabled)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    let checked = *checked;
                    this.update_shared_provider_metadata(
                        profile_id.clone(),
                        move |metadata| update(metadata, checked),
                        cx,
                    )
                }))
                .into_any_element()
        };
        let rows = vec![
            management_setting_row(
                management_locale_text("Auth header", "认证头", "認證標頭"),
                management_locale_text(
                    "Which header carries the Key. Auto follows the protocol.",
                    "用哪个请求头携带 Key；自动会按协议选择。",
                    "用哪個請求標頭攜帶 Key；自動會依協定選擇。",
                ),
                auth,
                cx,
            ),
            management_setting_row(
                "User-Agent",
                management_locale_text(
                    "The User-Agent sent upstream.",
                    "发往上游的 User-Agent。",
                    "發往上游的 User-Agent。",
                ),
                ua,
                cx,
            ),
            management_setting_row(
                management_locale_text("Route groups only", "仅通过路由组使用", "僅透過路由組使用"),
                management_locale_text(
                    "Keep these models out of Agent pickers; route groups still reach them.",
                    "不在 Agent 的模型选择器中显示，路由组仍可调用。",
                    "不在 Agent 的模型選擇器中顯示，路由組仍可呼叫。",
                ),
                switch(
                    "management-shared-provider-only-groups",
                    profile.metadata.only_through_route_groups,
                    pending,
                    |metadata, checked| metadata.only_through_route_groups = checked,
                    cx,
                ),
                cx,
            ),
            management_setting_row(
                management_locale_text(
                    "Refresh models on start",
                    "启动时刷新模型",
                    "啟動時重新整理模型",
                ),
                management_locale_text(
                    "Fetch the model list again each time Vibex starts.",
                    "每次 Vibex 启动时重新获取模型列表。",
                    "每次 Vibex 啟動時重新取得模型清單。",
                ),
                switch(
                    "management-shared-provider-auto-refresh",
                    profile.metadata.auto_refresh_models,
                    pending,
                    |metadata, checked| metadata.auto_refresh_models = checked,
                    cx,
                ),
                cx,
            ),
            management_setting_row(
                management_locale_text("Allow self-signed TLS", "允许自签证书", "允許自簽憑證"),
                if tls_locked {
                    management_locale_text(
                        "Only for custom and local Providers.",
                        "仅适用于自定义和本机供应商。",
                        "僅適用於自訂和本機供應商。",
                    )
                } else {
                    management_locale_text(
                        "Accept certificates no authority signed. Use only on networks you trust.",
                        "接受未经机构签发的证书，仅在可信网络中使用。",
                        "接受未經機構簽發的憑證，僅在可信網路中使用。",
                    )
                },
                switch(
                    "management-shared-provider-tls-invalid",
                    profile.metadata.tls_accept_invalid,
                    pending || tls_locked,
                    |metadata, checked| metadata.tls_accept_invalid = checked,
                    cx,
                ),
                cx,
            ),
        ];
        ws_section(
            management_locale_text("Behaviour", "行为", "行為"),
            Some(management_locale_text(
                "These apply as soon as they change.",
                "这些设置修改后立即生效。",
                "這些設定修改後立即生效。",
            )),
            None,
            management_settings_list(rows, cx),
            cx,
        )
    }

    fn render_workspace_provider_danger(
        &self,
        row: &SharedProviderRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.profile.id.clone();
        let label = row.profile.display_name.clone();
        h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_4()
            .px_4()
            .py_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().danger.opacity(0.4))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_sm().font_medium().child(management_locale_text(
                        "Delete this Provider",
                        "删除该供应商",
                        "刪除該供應商",
                    )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if row.agents.is_empty() {
                                management_locale_text(
                                    "No Agent uses it, so nothing else changes.",
                                    "没有 Agent 使用它，删除不会影响其他配置。",
                                    "沒有 Agent 使用它，刪除不會影響其他設定。",
                                )
                            } else {
                                management_locale_text(
                                    "Every Agent that uses it loses this model source.",
                                    "所有使用它的 Agent 都会失去这个模型来源。",
                                    "所有使用它的 Agent 都會失去這個模型來源。",
                                )
                            }),
                    ),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "management-provider-delete-{}",
                    id.as_str()
                )))
                .small()
                .danger()
                .label(management_locale_text("Delete…", "删除…", "刪除…"))
                .disabled(self.mutation.is_some())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.confirm_delete_shared_provider(id.clone(), label.clone(), window, cx)
                })),
            )
            .into_any_element()
    }
}

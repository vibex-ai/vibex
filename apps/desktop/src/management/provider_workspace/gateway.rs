//! The gateway's page.
//!
//! The gateway is the local address an Agent reaches when it uses a route
//! group. Its page answers, in tab order: is it serving (Overview), where each
//! request went and why (Routing), how well over time (Traffic), with which
//! accounts (Keys), and under which rules (Settings).

use super::*;

const GATEWAY_TABS: [GatewayDetailTab; 5] = [
    GatewayDetailTab::Overview,
    GatewayDetailTab::Routing,
    GatewayDetailTab::Traffic,
    GatewayDetailTab::Keys,
    GatewayDetailTab::Advanced,
];

fn gateway_tab_label(tab: GatewayDetailTab) -> &'static str {
    match tab {
        GatewayDetailTab::Keys => management_locale_text("Keys", "Key", "Key"),
        GatewayDetailTab::Traffic => management_locale_text("Traffic", "流量", "流量"),
        GatewayDetailTab::Routing => management_locale_text("Routing", "路由", "路由"),
        GatewayDetailTab::Advanced => management_locale_text("Settings", "设置", "設定"),
        GatewayDetailTab::Overview => management_locale_text("Overview", "概览", "概覽"),
    }
}

impl ManagementCenter {
    pub(super) fn render_workspace_gateway(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.schedule_gateway_cooldown_refresh(cx);
        let tab = self.gateway_detail_tab;
        let selected = GATEWAY_TABS
            .iter()
            .position(|item| *item == tab)
            .unwrap_or(0);
        let body = match tab {
            GatewayDetailTab::Keys => self.render_workspace_gateway_keys(window, cx),
            GatewayDetailTab::Routing => self.render_workspace_gateway_routes(cx),
            GatewayDetailTab::Traffic => self.render_workspace_gateway_traffic(cx),
            GatewayDetailTab::Advanced => self.render_workspace_gateway_settings(window, cx),
            GatewayDetailTab::Overview => self.render_workspace_gateway_overview(cx),
        };
        v_flex()
            .id("management-gateway")
            .w_full()
            .min_w_0()
            .gap_4()
            .child(self.render_workspace_gateway_header(window, cx))
            .child(ws_tabs(
                "management-gateway-detail-tabs",
                GATEWAY_TABS
                    .iter()
                    .map(|tab| gateway_tab_label(*tab))
                    .collect(),
                selected,
                |this, index, _, cx| {
                    if let Some(tab) = GATEWAY_TABS.get(index) {
                        this.gateway_detail_tab = *tab;
                        cx.notify();
                    }
                },
                cx,
            ))
            .child(body)
            .into_any_element()
    }

    fn render_workspace_gateway_header(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pending = self.mutation.is_some();
        let settings = self.gateway.settings.clone();
        let status = self.gateway.runtime.clone();
        let (state, color) = super::sidebar::workspace_gateway_state(&status.status, cx);

        let title = if self.gateway_name_editing {
            let input = self
                .gateway_name_input
                .get_or_insert_with(|| {
                    cx.new(|cx| {
                        InputState::new(window, cx).default_value(settings.display_name.clone())
                    })
                })
                .clone();
            v_flex()
                .w_full()
                .gap_1()
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .max_w(rems(20.0))
                                .child(Input::new(&input).w_full()),
                        )
                        .child(
                            Button::new("management-gateway-name-save")
                                .small()
                                .primary()
                                .label(management_locale_text("Save", "保存", "儲存"))
                                .disabled(pending)
                                .on_click(cx.listener(|this, _, _, cx| this.save_gateway_name(cx))),
                        )
                        .child(
                            Button::new("management-gateway-name-cancel")
                                .small()
                                .ghost()
                                .label(management_locale_text("Cancel", "取消", "取消"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.gateway_name_editing = false;
                                    this.gateway_name_error = None;
                                    cx.notify();
                                })),
                        ),
                )
                .children(
                    self.gateway_name_error.clone().map(|message| {
                        div().text_xs().text_color(cx.theme().danger).child(message)
                    }),
                )
                .into_any_element()
        } else {
            ws_title(gateway_display_name(&settings), Vec::new())
        };

        let address = status.base_url();
        let mut facts = vec![ws_state_fact(state, color)];
        match address.clone() {
            Some(address) => facts.push(ws_mono_fact(address, cx)),
            None => facts.push(
                div()
                    .child(management_locale_text("Not listening", "未监听", "未監聽"))
                    .into_any_element(),
            ),
        }
        let center = cx.weak_entity();
        let more = Button::new("management-gateway-more")
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
                let rename = {
                    let center = center.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| this.open_gateway_name_editor(window, cx))
                            .ok();
                    }
                };
                let copy = {
                    let address = address.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        if let Some(address) = address.clone() {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(address));
                        }
                    }
                };
                menu.item(
                    PopupMenuItem::new(management_locale_text("Rename…", "重命名…", "重新命名…"))
                        .icon(Icon::default().path("icons/vibex/pencil.svg"))
                        .on_click(rename),
                )
                .item(
                    PopupMenuItem::new(management_locale_text(
                        "Copy address",
                        "复制地址",
                        "複製位址",
                    ))
                    .icon(IconName::Copy)
                    .disabled(address.is_none())
                    .on_click(copy),
                )
            });
        let actions = h_flex()
            .items_center()
            .gap_2()
            .child(
                Switch::new("management-gateway-enabled")
                    .small()
                    .checked(settings.policy.enabled)
                    .disabled(pending)
                    .label(management_locale_text("Enabled", "启用", "啟用"))
                    .on_click(cx.listener(|this, checked, window, cx| {
                        if *checked {
                            this.set_gateway_enabled(true, cx);
                        } else {
                            this.confirm_disable_gateway(window, cx);
                        }
                    })),
            )
            .child(more)
            .into_any_element();
        ws_page_header(
            ws_icon_tile(
                "icons/vibex/server.svg",
                px(48.0),
                Some(cx.theme().primary),
                cx,
            ),
            title,
            ws_facts(facts, cx),
            actions,
        )
    }

    fn render_workspace_gateway_overview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let snapshot = self.gateway.clone();
        let now = vibex_core::unix_timestamp_ms();
        let uptime = snapshot.runtime.started_at_ms.map(|started| {
            let seconds = now.saturating_sub(started).max(0) / 1000;
            let (hours, minutes) = (seconds / 3600, (seconds % 3600) / 60);
            if hours > 0 {
                format!("{hours}h {minutes}m")
            } else {
                format!("{minutes}m")
            }
        });
        let total_keys = snapshot
            .upstreams
            .iter()
            .map(|upstream| upstream.keys.len())
            .sum::<usize>();
        let ready = snapshot.ready_key_count();
        let groups = snapshot
            .models
            .iter()
            .filter_map(|model| model.route_id.as_deref())
            .collect::<BTreeSet<_>>()
            .len();
        let cutoff = now - self.traffic_range.window_ms();
        let traffic = WorkspaceTraffic::collect(
            self.gateway_requests
                .iter()
                .filter(|request| request.created_at_ms >= cutoff),
        );
        let (state, color) = super::sidebar::workspace_gateway_state(&snapshot.runtime.status, cx);
        let tiles = ws_tile_row(vec![
            management_stat_tile(
                management_locale_text("Status", "状态", "狀態"),
                state.to_string(),
                uptime
                    .map(|uptime| {
                        format!(
                            "{} {uptime}",
                            management_locale_text("Up", "已运行", "已執行")
                        )
                    })
                    .unwrap_or_else(|| gateway_runtime_text(&snapshot.runtime)),
                Some(color),
                cx,
            ),
            management_stat_tile(
                management_locale_text("Keys ready", "可用 Key", "可用 Key"),
                format!("{ready}/{total_keys}"),
                if snapshot.cooling_key_count() > 0 || snapshot.invalid_key_count() > 0 {
                    format!(
                        "{} {} · {} {}",
                        snapshot.cooling_key_count(),
                        management_locale_text("cooling", "冷却中", "冷卻中"),
                        snapshot.invalid_key_count(),
                        management_locale_text("invalid", "已失效", "已失效")
                    )
                } else {
                    management_locale_text("All available", "全部可用", "全部可用").to_string()
                },
                (ready < total_keys).then(|| cx.theme().warning),
                cx,
            ),
            management_stat_tile(
                management_locale_text("Route groups", "路由组", "路由組"),
                groups.to_string(),
                format!(
                    "{} {}",
                    snapshot.models.len(),
                    management_locale_text("models served", "个模型对外提供", "個模型對外提供")
                ),
                None,
                cx,
            ),
            management_stat_tile(
                management_locale_text("Requests", "请求", "請求"),
                traffic.total.to_string(),
                format!(
                    "{} · {}",
                    traffic.success_caption(),
                    self.traffic_range.label()
                ),
                None,
                cx,
            ),
        ]);

        // What it serves: the model names an Agent can ask for, by group.
        let mut served = BTreeMap::<String, Vec<String>>::new();
        for model in &snapshot.models {
            let group = model
                .route_id
                .as_deref()
                .and_then(|route_id| {
                    self.model_routes
                        .iter()
                        .find(|route| route.id.as_str() == route_id)
                        .map(|route| route.display_name.clone())
                })
                .unwrap_or_else(|| model.provider_name.clone());
            served
                .entry(group)
                .or_default()
                .push(model.display_id.clone());
        }
        let mut served_list = v_flex().w_full().min_w_0();
        for (index, (group, models)) in served.iter().enumerate() {
            served_list = served_list.child(
                ws_row(
                    SharedString::from(format!("management-gateway-served-{index}")),
                    ws_icon_tile("icons/vibex/git-branch.svg", px(30.0), None, cx),
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(group.clone())
                        .into_any_element(),
                    Some(
                        h_flex()
                            .min_w_0()
                            .flex_wrap()
                            .gap_1()
                            .pt_0p5()
                            .children(models.iter().take(8).map(|model| {
                                Tag::color(ColorName::Neutral).xsmall().child(
                                    div()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(model.clone()),
                                )
                            }))
                            .when(models.len() > 8, |chips| {
                                chips.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("+{}", models.len() - 8)),
                                )
                            })
                            .into_any_element(),
                    ),
                    div().into_any_element(),
                    index == 0,
                    cx,
                )
                .into_any_element(),
            );
        }
        if served.is_empty() {
            served_list = served_list.child(ws_empty_line(
                management_locale_text(
                    "Nothing yet. Create a route group and its models are served here.",
                    "还没有。新建路由组后，它的模型就会由这里提供。",
                    "還沒有。新增路由組後，它的模型就會由這裡提供。",
                ),
                cx,
            ));
        }
        let address = snapshot.runtime.base_url();
        let connect = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_3()
            .p_3()
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted.opacity(0.5))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_sm()
                    .child(address.clone().unwrap_or_else(|| {
                        management_locale_text("Not listening", "未监听", "未監聽").to_string()
                    })),
            )
            .child(
                Button::new("management-gateway-copy-address")
                    .small()
                    .ghost()
                    .icon(IconName::Copy)
                    .label(management_locale_text("Copy", "复制", "複製"))
                    .disabled(address.is_none())
                    .on_click(move |_, _, cx| {
                        if let Some(address) = address.clone() {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(address));
                        }
                    }),
            );
        ws_stack(vec![
            tiles,
            ws_section(
                management_locale_text("Address", "地址", "位址"),
                Some(management_locale_text(
                    "An Agent that uses a route group points here instead of at a vendor. Vibex manages it.",
                    "使用路由组的 Agent 会指向这里，而不是厂商地址；该地址由 Vibex 管理。",
                    "使用路由組的 Agent 會指向這裡，而不是廠商位址；該位址由 Vibex 管理。",
                )),
                None,
                connect.into_any_element(),
                cx,
            ),
            ws_section(
                management_locale_text("Models served", "对外提供的模型", "對外提供的模型"),
                None,
                None,
                served_list.into_any_element(),
                cx,
            ),
        ])
    }

    fn render_workspace_gateway_keys(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let snapshot = self.gateway.clone();
        let pending = self.mutation.is_some();
        let mut sections = Vec::new();
        for upstream in &snapshot.upstreams {
            let profile = self
                .providers
                .iter()
                .find(|profile| profile.id == upstream.model_provider_profile_id)
                .cloned();
            let mut list = v_flex().w_full().min_w_0();
            for (index, key) in upstream.keys.iter().enumerate() {
                let preferred = snapshot
                    .settings
                    .policy
                    .preferred_keys
                    .get(&upstream.model_provider_profile_id)
                    .is_some_and(|credential| credential == &key.credential_id);
                let enabled = key.state != vibex_core::GatewayKeyState::Disabled;
                let center = cx.weak_entity();
                let profile_id = upstream.model_provider_profile_id.clone();
                let credential_id = key.credential_id.clone();
                let invalid = key.state == vibex_core::GatewayKeyState::Invalid;
                let more = Button::new(SharedString::from(format!(
                    "management-gateway-key-more-{}-{}",
                    upstream.model_provider_profile_id.as_str(),
                    key.credential_id.as_str()
                )))
                .small()
                .ghost()
                .icon(IconName::Ellipsis)
                .tooltip(management_locale_text(
                    "More actions",
                    "更多操作",
                    "更多操作",
                ))
                .disabled(pending)
                .dropdown_menu(move |menu, _, _| {
                    let prefer = {
                        let center = center.clone();
                        let profile_id = profile_id.clone();
                        let credential_id = credential_id.clone();
                        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                            center
                                .update(cx, |this, cx| {
                                    this.set_gateway_key_preferred(
                                        profile_id.clone(),
                                        credential_id.clone(),
                                        !preferred,
                                        cx,
                                    )
                                })
                                .ok();
                        }
                    };
                    let replace = {
                        let center = center.clone();
                        let profile_id = profile_id.clone();
                        let credential_id = credential_id.clone();
                        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                            center
                                .update(cx, |this, cx| {
                                    this.gateway_key_replace_target =
                                        Some((profile_id.clone(), credential_id.clone()));
                                    this.gateway_key_replace_input = None;
                                    this.gateway_key_replace_error = None;
                                    cx.notify();
                                })
                                .ok();
                        }
                    };
                    let remove = {
                        let center = center.clone();
                        let profile_id = profile_id.clone();
                        let credential_id = credential_id.clone();
                        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                            center
                                .update(cx, |this, cx| {
                                    this.confirm_remove_gateway_key(
                                        profile_id.clone(),
                                        credential_id.clone(),
                                        window,
                                        cx,
                                    )
                                })
                                .ok();
                        }
                    };
                    menu.item(
                        PopupMenuItem::new(management_locale_text(
                            "Try this Key first",
                            "优先尝试该 Key",
                            "優先嘗試該 Key",
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
                    .when(preferred, |row| {
                        row.child(
                            Tag::color(ColorName::Blue)
                                .xsmall()
                                .child(management_locale_text("Preferred", "优先", "優先")),
                        )
                    })
                    .child(gateway_key_state_badge(key, cx))
                    .when(invalid, |row| {
                        row.child(
                            Button::new(SharedString::from(format!(
                                "management-gateway-replace-key-{}-{}",
                                upstream.model_provider_profile_id.as_str(),
                                key.credential_id.as_str()
                            )))
                            .small()
                            .outline()
                            .label(management_locale_text("Replace…", "更换…", "更換…"))
                            .disabled(pending)
                            .on_click(cx.listener({
                                let profile = upstream.model_provider_profile_id.clone();
                                let credential = key.credential_id.clone();
                                move |this, _, _, cx| {
                                    this.gateway_key_replace_target =
                                        Some((profile.clone(), credential.clone()));
                                    this.gateway_key_replace_input = None;
                                    this.gateway_key_replace_error = None;
                                    cx.notify();
                                }
                            })),
                        )
                    })
                    .child(
                        Switch::new(SharedString::from(format!(
                            "management-gateway-key-enabled-{}-{}",
                            upstream.model_provider_profile_id.as_str(),
                            key.credential_id.as_str()
                        )))
                        .small()
                        .checked(enabled)
                        .disabled(pending)
                        .on_click(cx.listener({
                            let profile = upstream.model_provider_profile_id.clone();
                            let credential = key.credential_id.clone();
                            move |this, checked, _, cx| {
                                this.set_gateway_key_enabled(
                                    profile.clone(),
                                    credential.clone(),
                                    *checked,
                                    cx,
                                )
                            }
                        })),
                    )
                    .child(more)
                    .into_any_element();
                list = list.child(ws_row(
                    SharedString::from(format!(
                        "management-gateway-key-{}-{}",
                        upstream.model_provider_profile_id.as_str(),
                        key.credential_id.as_str()
                    )),
                    ws_icon_tile("icons/vibex/file-key.svg", px(30.0), None, cx),
                    self.render_provider_key_value(
                        &upstream.model_provider_profile_id,
                        &key.credential_id,
                        &key.masked_hint,
                        &key.display_name,
                        true,
                        cx,
                    ),
                    Some(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} {}",
                                gateway_token_count(key.used_tokens),
                                management_locale_text("tokens used", "Token 已用", "Token 已用")
                            ))
                            .into_any_element(),
                    ),
                    trailing,
                    index == 0,
                    cx,
                ));
                list = list
                    .child(self.render_gateway_key_limits(key, window, cx))
                    .child(self.render_gateway_key_replace(
                        &upstream.model_provider_profile_id,
                        &key.credential_id,
                        window,
                        cx,
                    ));
            }
            if upstream.keys.is_empty() {
                list = list.child(div().py_3().text_sm().text_color(cx.theme().warning).child(
                    management_locale_text(
                        "No Key: this Provider cannot answer a request.",
                        "没有 Key：该供应商无法响应请求。",
                        "沒有 Key：該供應商無法回應請求。",
                    ),
                ));
            }
            let open_id = upstream.model_provider_profile_id.as_str().to_string();
            let head = h_flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap_3()
                .pb_2()
                .child(match profile.as_ref() {
                    Some(profile) => provider_logo_tile(profile, px(32.0), cx),
                    None => ws_icon_tile("icons/vibex/database.svg", px(32.0), None, cx),
                })
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .font_semibold()
                                .child(upstream.display_name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{} {} · {} Key",
                                    upstream.model_count,
                                    management_locale_text("models", "个模型", "個模型"),
                                    upstream.keys.len()
                                )),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "management-gateway-open-provider-{open_id}"
                    )))
                    .small()
                    .ghost()
                    .label(management_locale_text(
                        "Open Provider",
                        "打开供应商",
                        "開啟供應商",
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.provider_detail_tab = ProviderDetailTab::Keys;
                        this.select_workspace_provider(open_id.clone(), cx)
                    })),
                );
            sections.push(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(theme::semantic_color("card", cx.theme().is_dark()).opacity(0.72))
                    .px_4()
                    .py_3()
                    .child(head)
                    .child(list)
                    .into_any_element(),
            );
        }
        if sections.is_empty() {
            sections.push(ws_section(
                management_locale_text("Keys", "Key", "Key"),
                None,
                None,
                ws_empty_line(
                    management_locale_text(
                        "No Provider is in a route group yet. Keys appear here once one is.",
                        "还没有供应商加入路由组；加入后它的 Key 会出现在这里。",
                        "還沒有供應商加入路由組；加入後它的 Key 會出現在這裡。",
                    ),
                    cx,
                ),
                cx,
            ));
        }
        v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(management_locale_text(
                        "Every Key of every Provider a route group uses. The gateway rotates between those turned on and rests the ones that fail.",
                        "路由组所用供应商的全部 Key。网关在已启用的 Key 之间轮换，并让失败的 Key 冷却。",
                        "路由組所用供應商的全部 Key。閘道在已啟用的 Key 之間輪換，並讓失敗的 Key 冷卻。",
                    )),
            )
            .children(sections)
            .into_any_element()
    }

    fn render_workspace_gateway_traffic(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let cutoff = vibex_core::unix_timestamp_ms() - self.traffic_range.window_ms();
        let rows = self
            .gateway_requests
            .iter()
            .filter(|request| request.created_at_ms >= cutoff)
            .collect::<Vec<_>>();
        let traffic = WorkspaceTraffic::collect(rows.iter().copied());
        let failovers = rows
            .iter()
            .filter(|request| request.outcome == vibex_core::GatewayRequestOutcome::Failover)
            .count();
        let input = rows.iter().map(|request| request.input_tokens).sum::<u64>();
        let output = rows
            .iter()
            .map(|request| request.output_tokens)
            .sum::<u64>();
        let cached = rows
            .iter()
            .map(|request| request.cached_read_tokens)
            .sum::<u64>();
        let top = |key: fn(&vibex_core::GatewayRequestSummary) -> String| {
            let mut counts = BTreeMap::<String, usize>::new();
            for request in &rows {
                *counts.entry(key(request)).or_default() += 1;
            }
            let mut counts = counts.into_iter().collect::<Vec<_>>();
            counts.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
            counts.into_iter().take(5).collect::<Vec<_>>()
        };
        let ranking = |title: &'static str, entries: Vec<(String, usize)>, cx: &App| {
            let max = entries.first().map(|(_, count)| *count).unwrap_or(1).max(1);
            v_flex()
                .flex_1()
                .min_w(rems(16.0))
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(cx.theme().muted_foreground)
                        .child(title),
                )
                .children(entries.into_iter().map(|(name, count)| {
                    v_flex()
                        .w_full()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .gap_2()
                                .text_xs()
                                .child(div().min_w_0().truncate().child(name))
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(count.to_string()),
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .h(px(4.0))
                                .rounded_full()
                                .bg(cx.theme().muted)
                                .child(
                                    div()
                                        .h_full()
                                        .w(gpui::relative(count as f32 / max as f32))
                                        .rounded_full()
                                        .bg(cx.theme().primary.opacity(0.7)),
                                ),
                        )
                }))
                .when(rows.is_empty(), |list| {
                    list.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("—"),
                    )
                })
                .into_any_element()
        };
        let rankings = h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .items_start()
            .gap_6()
            .child(ranking(
                management_locale_text("Top Providers", "请求最多的供应商", "請求最多的供應商"),
                top(|request| request.provider_name.clone()),
                cx,
            ))
            .child(ranking(
                management_locale_text("Top models", "请求最多的模型", "請求最多的模型"),
                top(|request| request.model_requested.clone()),
                cx,
            ));
        ws_section(
            management_locale_text("Traffic", "流量", "流量"),
            Some(management_locale_text(
                "What the gateway observed in this window. No cost, price or quota is tracked.",
                "网关在该时间窗口内的观测数据；不统计费用、价格或额度。",
                "閘道在該時間窗口內的觀測資料；不統計費用、價格或額度。",
            )),
            Some(self.workspace_range_menu("management-gateway-dashboard-range", cx)),
            v_flex()
                .w_full()
                .gap_4()
                .child(traffic.tiles(cx))
                .child(ws_tile_row(vec![
                    management_stat_tile(
                        management_locale_text("Failovers", "故障转移", "容錯移轉"),
                        failovers.to_string(),
                        format!(
                            "{} {}",
                            self.gateway_inflight.len(),
                            management_locale_text("in flight", "个进行中", "個進行中")
                        ),
                        None,
                        cx,
                    ),
                    management_stat_tile(
                        management_locale_text("Tokens in", "输入 Token", "輸入 Token"),
                        gateway_token_count(input),
                        format!(
                            "{} {}",
                            gateway_token_count(cached),
                            management_locale_text("from cache", "来自缓存", "來自快取")
                        ),
                        None,
                        cx,
                    ),
                    management_stat_tile(
                        management_locale_text("Tokens out", "输出 Token", "輸出 Token"),
                        gateway_token_count(output),
                        String::new(),
                        None,
                        cx,
                    ),
                ]))
                .child(Separator::horizontal())
                .child(rankings)
                .into_any_element(),
            cx,
        )
    }

    fn render_workspace_gateway_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pending = self.mutation.is_some();
        let policy = self.gateway.settings.policy.clone();

        // Token ceiling: the window and the count are edited together, since
        // neither means anything alone.
        let stored = policy.token_limit;
        let window_choice = self
            .gateway_token_limit_window
            .or_else(|| stored.map(|limit| limit.window))
            .unwrap_or(vibex_core::GatewayTokenWindow::Day);
        let limit_input = self
            .gateway_token_limit_input
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    InputState::new(window, cx).placeholder(management_locale_text(
                        "Tokens",
                        "Token 数",
                        "Token 數",
                    ))
                })
            })
            .clone();
        let center = cx.weak_entity();
        let window_menu = Button::new("management-gateway-token-window")
            .outline()
            .label(gateway_token_window_label(window_choice))
            .dropdown_caret(true)
            .disabled(pending)
            .dropdown_menu(move |mut menu, _, _| {
                for choice in vibex_core::GatewayTokenWindow::ALL {
                    let center = center.clone();
                    menu = menu.item(
                        PopupMenuItem::new(gateway_token_window_label(choice))
                            .checked(choice == window_choice)
                            .on_click(move |_, _, cx| {
                                center
                                    .update(cx, |this, cx| {
                                        this.gateway_token_limit_window = Some(choice);
                                        cx.notify();
                                    })
                                    .ok();
                            }),
                    );
                }
                menu
            });
        let limit = ws_section(
            management_locale_text("Token ceiling", "Token 上限", "Token 上限"),
            Some(management_locale_text(
                "A local ceiling for the whole gateway, counted from the tokens it observed. It is not the vendor's quota. A route group may set its own.",
                "整个网关的本地上限，按网关观测到的 Token 计数，不等于厂商额度；路由组可以另设上限。",
                "整個閘道的本機上限，按閘道觀測到的 Token 計數，不等於廠商額度；路由組可以另設上限。",
            )),
            Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(match stored {
                        Some(limit) => format!(
                            "{} {} / {}",
                            management_locale_text("Now", "当前", "目前"),
                            gateway_token_count(limit.tokens),
                            gateway_token_window_label(limit.window)
                        ),
                        None => {
                            management_locale_text("Not limited", "不限制", "不限制").to_string()
                        }
                    })
                    .into_any_element(),
            ),
            v_flex()
                .w_full()
                .gap_2()
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&limit_input).w_full()),
                        )
                        .child(window_menu)
                        .child(
                            Button::new("management-gateway-token-limit-save")
                                .primary()
                                .label(management_locale_text("Save", "保存", "儲存"))
                                .disabled(pending)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.save_gateway_token_limit(cx)),
                                ),
                        )
                        .when(stored.is_some(), |row| {
                            row.child(
                                Button::new("management-gateway-token-limit-clear")
                                    .ghost()
                                    .label(management_locale_text("Remove…", "移除…", "移除…"))
                                    .disabled(pending)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.confirm_clear_gateway_token_limit(window, cx)
                                    })),
                            )
                        }),
                )
                .children(
                    self.gateway_token_limit_error.clone().map(|message| {
                        div().text_xs().text_color(cx.theme().danger).child(message)
                    }),
                )
                .into_any_element(),
            cx,
        );

        // Protocol translation, applied immediately.
        let translate = policy.translate;
        let translation = ws_section(
            management_locale_text("Protocol translation", "协议转换", "協定轉換"),
            Some(management_locale_text(
                "Used only when the Agent and the upstream speak different protocols. A direction turned off answers 400 instead of sending a mangled request.",
                "只有 Agent 与上游协议不同时才会转换。关闭某个方向后，需要它的请求会直接返回 400，而不是被转换坏后继续发送。",
                "只有 Agent 與上游協定不同時才會轉換。關閉某個方向後，需要它的請求會直接回傳 400，而不是被轉換壞後繼續傳送。",
            )),
            None,
            management_settings_list(
                GatewayTranslation::ALL
                    .into_iter()
                    .map(|direction| {
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_4()
                            .py_2p5()
                            .child(div().flex_1().min_w_0().text_sm().child(direction.label()))
                            .child(
                                Switch::new(SharedString::from(format!(
                                    "management-gateway-translate-{}",
                                    direction.id()
                                )))
                                .small()
                                .checked(direction.allowed(&translate))
                                .disabled(pending)
                                .on_click(cx.listener(
                                    move |this, checked, _, cx| {
                                        this.set_gateway_translation(direction, *checked, cx)
                                    },
                                )),
                            )
                            .into_any_element()
                    })
                    .collect(),
                cx,
            ),
            cx,
        );

        // Alerts and default timeouts.
        let inputs = self.workspace_gateway_default_inputs(window, cx);
        let labels = [
            (
                management_locale_text("Error-rate alert", "错误率告警", "錯誤率警示"),
                "%",
            ),
            (
                management_locale_text("Latency alert", "延迟告警", "延遲警示"),
                "ms",
            ),
            (
                management_locale_text("Connect timeout", "连接超时", "連線逾時"),
                "ms",
            ),
            (
                management_locale_text("Request timeout", "请求超时", "請求逾時"),
                "ms",
            ),
            (
                management_locale_text("Stream idle timeout", "流空闲超时", "串流閒置逾時"),
                "ms",
            ),
            (
                management_locale_text("Rate-limit cooldown", "限流冷却", "限流冷卻"),
                "ms",
            ),
            (
                management_locale_text("Upstream-error cooldown", "上游错误冷却", "上游錯誤冷卻"),
                "ms",
            ),
            (
                management_locale_text("Proxy-error cooldown", "代理错误冷却", "代理錯誤冷卻"),
                "ms",
            ),
        ];
        let mut grid = h_flex().w_full().min_w_0().flex_wrap().gap_4();
        for ((label, unit), input) in labels.into_iter().zip(inputs.iter()) {
            grid = grid.child(
                div().w(rems(12.0)).child(ws_field(
                    label,
                    None,
                    Input::new(input)
                        .w_full()
                        .suffix(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(unit),
                        )
                        .into_any_element(),
                    cx,
                )),
            );
        }
        let defaults = ws_section(
            management_locale_text("Alerts and timeouts", "告警与超时", "警示與逾時"),
            Some(management_locale_text(
                "Thresholds that raise a warning and the timeouts used when a Provider sets none. Empty leaves a value unset.",
                "触发告警的阈值，以及供应商未设置时使用的超时；留空表示不设置。",
                "觸發警示的閾值，以及供應商未設定時使用的逾時；留空表示不設定。",
            )),
            None,
            v_flex()
                .w_full()
                .gap_2()
                .child(grid)
                .child(ws_form_footer(
                    self.gateway_default_error.clone(),
                    vec![
                        Button::new("management-gateway-defaults-save")
                            .primary()
                            .label(management_locale_text("Save", "保存", "儲存"))
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, _, cx| this.save_gateway_defaults(cx)))
                            .into_any_element(),
                    ],
                    cx,
                ))
                .into_any_element(),
            cx,
        );
        ws_stack(vec![limit, translation, defaults])
    }

    /// The alert and timeout drafts, created from the saved policy on first
    /// view.
    fn workspace_gateway_default_inputs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Entity<InputState>> {
        if let Some(inputs) = self.gateway_default_inputs.clone() {
            return inputs;
        }
        let policy = self.gateway.settings.policy.clone();
        let value = |value: Option<u64>| value.map(|value| value.to_string()).unwrap_or_default();
        let values = [
            policy
                .error_rate_warn_percent
                .map(|value| value.to_string())
                .unwrap_or_default(),
            value(policy.latency_warn_ms),
            value(policy.default_connect_timeout_ms),
            value(policy.default_request_timeout_ms),
            value(policy.default_stream_idle_timeout_ms),
            value(policy.cooldown_overrides_ms.get("rate").copied()),
            value(policy.cooldown_overrides_ms.get("upstream").copied()),
            value(policy.cooldown_overrides_ms.get("proxy").copied()),
        ];
        let inputs = values
            .into_iter()
            .map(|value| cx.new(|cx| InputState::new(window, cx).default_value(value)))
            .collect::<Vec<_>>();
        self.gateway_default_inputs = Some(inputs.clone());
        inputs
    }
}

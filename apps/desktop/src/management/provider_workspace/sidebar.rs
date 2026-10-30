//! The Provider workspace's left column.
//!
//! One scrolling list, in the order a request travels backwards: the gateway
//! that serves the groups, the Providers that answer, then the route groups
//! that compose them. Two foldable sections replace the old pair of tabs, so
//! the column never hides half of what the page owns.

use super::*;

impl ManagementCenter {
    pub(in crate::management) fn render_provider_workspace_sidebar(
        &mut self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.schedule_gateway_cooldown_refresh(cx);
        let query = self.provider_search.read(cx).value().trim().to_lowercase();

        let providers = self.workspace_sidebar_providers(&query);
        let routes = shared_route_group_rows(&self.model_routes, &self.agent_model_route_bindings)
            .into_iter()
            .filter(|row| {
                query.is_empty() || row.route.display_name.to_lowercase().contains(&query)
            })
            .collect::<Vec<_>>();

        // A selection that points at a record which no longer exists falls
        // back to the gateway, so the detail pane never describes a deleted
        // record.
        let provider_selected = self.providers_sidebar_tab == ProviderSidebarTab::Providers;
        if provider_selected
            && self.selected_global_provider_id.as_deref() != Some(GATEWAY_ROW_ID)
            && !self.shared_providers().iter().any(|row| {
                Some(row.profile.id.as_str()) == self.selected_global_provider_id.as_deref()
            })
        {
            self.selected_global_provider_id = Some(GATEWAY_ROW_ID.to_string());
        }
        if !provider_selected
            && !self
                .model_routes
                .iter()
                .any(|route| Some(route.id.as_str()) == self.selected_global_route_id.as_deref())
        {
            self.providers_sidebar_tab = ProviderSidebarTab::Providers;
            self.selected_global_provider_id = Some(GATEWAY_ROW_ID.to_string());
            self.selected_global_route_id = None;
        }

        let mut list = v_flex().w_full().min_w_0().gap_1();
        list = list.child(self.render_workspace_gateway_row(cx));

        // Providers.
        let provider_total = self.providers.len();
        list = list.child(self.render_workspace_section_head(
            "management-workspace-providers-head",
            management_locale_text("Providers", "供应商", "供應商"),
            if query.is_empty() && self.provider_list_filter == ProviderListFilter::All {
                provider_total.to_string()
            } else {
                format!("{}/{provider_total}", providers.len())
            },
            self.workspace_providers_folded,
            Some(self.render_workspace_provider_filter_menu(cx)),
            |this, cx| {
                this.workspace_providers_folded = !this.workspace_providers_folded;
                cx.notify();
            },
            cx,
        ));
        if !self.workspace_providers_folded {
            if providers.is_empty() {
                list = list.child(self.render_workspace_sidebar_empty(
                    if query.is_empty() && self.provider_list_filter == ProviderListFilter::All {
                        management_locale_text(
                            "No Providers yet. Add one to give Agents a model source.",
                            "还没有供应商。添加后 Agent 才有可用的模型来源。",
                            "還沒有供應商。新增後 Agent 才有可用的模型來源。",
                        )
                    } else {
                        management_locale_text(
                            "No Provider matches.",
                            "没有匹配的供应商。",
                            "沒有符合的供應商。",
                        )
                    },
                    cx,
                ));
            }
            for row in &providers {
                list = list.child(self.render_workspace_provider_row(row, cx));
            }
        }

        // Route groups.
        let suggestions = self.recommended_route_groups().len();
        list = list.child(self.render_workspace_section_head(
            "management-workspace-routes-head",
            management_locale_text("Route groups", "路由组", "路由組"),
            routes.len().to_string(),
            self.workspace_routes_folded,
            (suggestions > 0).then(|| {
                Button::new("management-route-group-suggestions-show")
                    .xsmall()
                    .ghost()
                    .icon(Icon::default().path("icons/vibex/sparkles.svg"))
                    .label(suggestions.to_string())
                    .tooltip(management_locale_text(
                        "Show suggested groups",
                        "显示推荐路由组",
                        "顯示推薦路由組",
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.route_group_recommendations_open =
                            !this.route_group_recommendations_open;
                        cx.notify();
                    }))
                    .into_any_element()
            }),
            |this, cx| {
                this.workspace_routes_folded = !this.workspace_routes_folded;
                cx.notify();
            },
            cx,
        ));
        if !self.workspace_routes_folded {
            if routes.is_empty() {
                list = list.child(self.render_workspace_sidebar_empty(
                    management_locale_text(
                        "No route groups. A group joins models from several Providers under one name.",
                        "还没有路由组。路由组把多个供应商的模型合成一个模型名。",
                        "還沒有路由組。路由組把多個供應商的模型合成一個模型名。",
                    ),
                    cx,
                ));
            }
            for row in &routes {
                list = list.child(self.render_workspace_route_row(row, cx));
            }
        }

        let drawer = (self.route_group_recommendations_open && suggestions > 0)
            .then(|| self.render_route_group_recommendations_drawer(cx));
        let sidebar_size_target = cx.entity().downgrade();
        v_flex()
            .id("management-provider-page-sidebar")
            .w_full()
            .h_full()
            .min_w_0()
            .on_prepaint(move |bounds, _, cx| {
                let _ = sidebar_size_target.update(cx, |this, cx| {
                    this.record_route_group_recommendations_body_height(
                        f32::from(bounds.size.height),
                        cx,
                    )
                });
            })
            .child(
                v_flex()
                    .w_full()
                    .min_h_0()
                    .flex_1()
                    .gap_2()
                    .p_3()
                    .child(self.render_workspace_sidebar_toolbar(cx))
                    .child(self.render_native_import_report(cx))
                    .child(
                        div()
                            .id("management-provider-section-list")
                            .w_full()
                            .min_h_0()
                            .flex_1()
                            .overflow_y_scrollbar()
                            .scroll_gutter()
                            .child(list),
                    )
                    .child(self.render_catalog_status_row(cx)),
            )
            .children(drawer)
            .into_any_element()
    }

    /// The Providers the column lists, filtered, favourites first.
    fn workspace_sidebar_providers(&self, query: &str) -> Vec<SharedProviderRow> {
        let mut rows =
            self.shared_providers()
                .into_iter()
                .filter(|row| {
                    query.is_empty()
                        || row.profile.display_name.to_lowercase().contains(query)
                        || row
                            .profile
                            .endpoints
                            .iter()
                            .any(|endpoint| endpoint.url.to_lowercase().contains(query))
                        || row
                            .profile
                            .configured_models
                            .iter()
                            .any(|model| model.id.to_lowercase().contains(query))
                        || row.profile.credentials.iter().any(|credential| {
                            credential.display_name.to_lowercase().contains(query)
                        })
                })
                .filter(|row| match self.provider_list_filter {
                    ProviderListFilter::All => true,
                    ProviderListFilter::Enabled => {
                        row.profile.status == vibex_core::ModelProviderProfileStatus::Enabled
                    }
                    ProviderListFilter::Disabled => {
                        row.profile.status == vibex_core::ModelProviderProfileStatus::Disabled
                    }
                    ProviderListFilter::NeedsKey => row.profile.credentials.is_empty(),
                    ProviderListFilter::Warning => {
                        row.profile.credentials.is_empty()
                            || !row
                                .profile
                                .configured_models
                                .iter()
                                .any(|model| model.enabled)
                    }
                    ProviderListFilter::Shared => row.agents.len() > 1,
                })
                .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            let left_favorite = self.provider_favorites.contains(left.profile.id.as_str());
            let right_favorite = self.provider_favorites.contains(right.profile.id.as_str());
            right_favorite
                .cmp(&left_favorite)
                .then_with(|| match self.provider_sort {
                    ProviderSort::Name => left
                        .profile
                        .display_name
                        .to_lowercase()
                        .cmp(&right.profile.display_name.to_lowercase()),
                    ProviderSort::RecentlyUpdated => {
                        right.profile.updated_at_ms.cmp(&left.profile.updated_at_ms)
                    }
                    ProviderSort::MostAgents => right.agents.len().cmp(&left.agents.len()),
                    ProviderSort::Health => self
                        .provider_health_rank(&right.profile)
                        .cmp(&self.provider_health_rank(&left.profile)),
                })
        });
        rows
    }

    /// The search field and the one command that adds a record.
    fn render_workspace_sidebar_toolbar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let importable_agents = self
            .snapshot
            .agents
            .iter()
            .filter(|agent| agent.added && vibex_core::is_user_visible_agent(&agent.id))
            .filter(|agent| vibex_core::supports_native_provider_import(&agent.id))
            .map(|agent| agent.id.as_str().to_string())
            .collect::<Vec<_>>();
        let importing = matches!(
            &self.mutation,
            Some(ManagementMutation::ProviderPreview(action))
                if action == "native-import" || action == "native-scan"
        );
        let center = cx.weak_entity();
        let add = Button::new("management-provider-page-add")
            .small()
            .icon(IconName::Plus)
            .label(management_locale_text("New", "新建", "新建"))
            .dropdown_caret(true)
            .loading(importing)
            .dropdown_menu(move |menu, _, _| {
                let provider = {
                    let center = center.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.open_provider_create_page(None, window, cx)
                            })
                            .ok();
                    }
                };
                let group = {
                    let center = center.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| this.open_route_group_page_editor(window, cx))
                            .ok();
                    }
                };
                let import = {
                    let center = center.clone();
                    let agents = importable_agents.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.import_shared_providers(agents.clone(), cx)
                            })
                            .ok();
                    }
                };
                menu.item(
                    PopupMenuItem::new(management_locale_text("Provider…", "供应商…", "供應商…"))
                        .icon(Icon::default().path("icons/vibex/database.svg"))
                        .on_click(provider),
                )
                .item(
                    PopupMenuItem::new(management_locale_text(
                        "Route group…",
                        "路由组…",
                        "路由組…",
                    ))
                    .icon(Icon::default().path("icons/vibex/git-branch.svg"))
                    .on_click(group),
                )
                .separator()
                .item(
                    PopupMenuItem::new(management_locale_text(
                        "Import existing configuration",
                        "导入已有配置",
                        "匯入既有配置",
                    ))
                    .icon(Icon::default().path("icons/vibex/import.svg"))
                    .on_click(import),
                )
            });
        h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(management_search_input_compact(&self.provider_search, cx)),
            )
            .child(add)
            .into_any_element()
    }

    /// The filter and sort for the Provider section, as one menu.
    fn render_workspace_provider_filter_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let active_filter = self.provider_list_filter;
        let active_sort = self.provider_sort;
        let center = cx.weak_entity();
        let narrowed = active_filter != ProviderListFilter::All;
        Button::new("management-provider-filter")
            .xsmall()
            .ghost()
            .when(narrowed, |button| button.selected(true))
            .icon(Icon::default().path("icons/vibex/list-checks.svg"))
            .tooltip(management_locale_text(
                "Filter and sort",
                "筛选与排序",
                "篩選與排序",
            ))
            .accessibility_label(management_locale_text(
                "Filter and sort",
                "筛选与排序",
                "篩選與排序",
            ))
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.label(management_locale_text("Show", "显示", "顯示"));
                for filter in ProviderListFilter::ALL {
                    let center = center.clone();
                    menu = menu.item(
                        PopupMenuItem::new(filter.label())
                            .checked(filter == active_filter)
                            .on_click(move |_, _, cx| {
                                center
                                    .update(cx, |this, cx| {
                                        this.provider_list_filter = filter;
                                        cx.notify();
                                    })
                                    .ok();
                            }),
                    );
                }
                menu = menu
                    .separator()
                    .label(management_locale_text("Sort by", "排序", "排序"));
                for sort in ProviderSort::ALL {
                    let center = center.clone();
                    menu = menu.item(
                        PopupMenuItem::new(sort.label())
                            .checked(sort == active_sort)
                            .on_click(move |_, _, cx| {
                                center
                                    .update(cx, |this, cx| {
                                        this.provider_sort = sort;
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

    /// A foldable section heading: chevron, name, count, one trailing tool.
    #[allow(clippy::too_many_arguments)]
    fn render_workspace_section_head(
        &self,
        id: &'static str,
        title: &'static str,
        count: String,
        folded: bool,
        tool: Option<AnyElement>,
        on_toggle: impl Fn(&mut ManagementCenter, &mut Context<ManagementCenter>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(id)
            .w_full()
            .min_w_0()
            .items_center()
            .gap_1()
            .pt_3()
            .pb_1()
            .px_1()
            .child(
                h_flex()
                    .id(SharedString::from(format!("{id}-toggle")))
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| on_toggle(this, cx)))
                    .child(
                        Icon::new(if folded {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .size_3p5()
                        .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.8))
                            .child(count),
                    ),
            )
            .children(tool)
            .into_any_element()
    }

    fn render_workspace_sidebar_empty(&self, text: &'static str, cx: &App) -> AnyElement {
        div()
            .w_full()
            .px_2()
            .py_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text)
            .into_any_element()
    }

    /// The pinned gateway row.
    fn render_workspace_gateway_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let gateway = &self.gateway;
        let selected = self.selected_global_provider_id.as_deref() == Some(GATEWAY_ROW_ID)
            && !self.provider_create_page_open
            && !self.route_group_page_open;
        let (state, color) = workspace_gateway_state(&gateway.runtime.status, cx);
        let subtitle = gateway.runtime.base_url().unwrap_or_else(|| {
            management_locale_text("Not listening", "未监听", "未監聽").to_string()
        });
        workspace_nav_row(
            SharedString::from("management-global-provider-row-gateway"),
            selected,
            ws_icon_tile(
                "icons/vibex/server.svg",
                px(30.0),
                Some(cx.theme().primary),
                cx,
            ),
            gateway_display_name(&gateway.settings),
            div()
                .min_w_0()
                .truncate()
                .font_family(cx.theme().mono_font_family.clone())
                .child(subtitle)
                .into_any_element(),
            Some(ws_state_fact(state, color)),
            cx.listener(|this, _, _, cx| this.select_workspace_gateway(cx)),
            cx,
        )
    }

    fn render_workspace_provider_row(
        &self,
        row: &SharedProviderRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.profile.id.as_str().to_string();
        let selected = self.providers_sidebar_tab == ProviderSidebarTab::Providers
            && self.selected_global_provider_id.as_deref() == Some(id.as_str())
            && !self.provider_create_page_open
            && !self.route_group_page_open;
        let enabled = row.profile.status == vibex_core::ModelProviderProfileStatus::Enabled;
        let models = row
            .profile
            .configured_models
            .iter()
            .filter(|model| model.enabled)
            .count();
        let (health, health_status) = self.shared_provider_health_label(&row.profile);
        let favorite = self.provider_favorites.contains(id.as_str());
        let subtitle = if row.profile.credentials.is_empty() {
            div()
                .text_color(cx.theme().warning)
                .child(management_locale_text(
                    "Needs a Key",
                    "需要 Key",
                    "需要 Key",
                ))
                .into_any_element()
        } else {
            div()
                .min_w_0()
                .truncate()
                .child(format!(
                    "{models} {} · {} {}",
                    management_locale_text("models", "个模型", "個模型"),
                    row.agents.len(),
                    management_locale_text("Agents", "个 Agent", "個 Agent"),
                ))
                .into_any_element()
        };
        let trailing = if !enabled {
            Tag::color(ColorName::Neutral)
                .xsmall()
                .child(management_locale_text("Off", "已停用", "已停用"))
                .into_any_element()
        } else {
            h_flex()
                .items_center()
                .gap_1()
                .when(favorite, |mark| {
                    mark.child(
                        Icon::new(IconName::StarFill)
                            .size_3()
                            .text_color(cx.theme().warning),
                    )
                })
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "management-provider-health-{id}"
                        )))
                        .size(px(7.0))
                        .rounded_full()
                        .bg(management_provider_health_color(health_status, cx))
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(health).build(window, cx)
                        }),
                )
                .into_any_element()
        };
        let select_id = id.clone();
        workspace_nav_row(
            SharedString::from(format!("management-global-provider-row-{id}")),
            selected,
            div()
                .when(!enabled, |mark| mark.opacity(0.55))
                .child(provider_logo_tile(&row.profile, px(30.0), cx))
                .into_any_element(),
            row.profile.display_name.clone(),
            subtitle,
            Some(trailing),
            cx.listener(move |this, _, _, cx| {
                this.select_workspace_provider(select_id.clone(), cx)
            }),
            cx,
        )
    }

    fn render_workspace_route_row(
        &self,
        row: &SharedRouteGroupRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.route.id.as_str().to_string();
        let selected = self.providers_sidebar_tab == ProviderSidebarTab::RouteGroups
            && self.selected_global_route_id.as_deref() == Some(id.as_str())
            && !self.provider_create_page_open
            && !self.route_group_page_open;
        let enabled = row.route.status == vibex_core::RouteStatus::Enabled;
        let subtitle = format!(
            "{} {} · {}",
            row.route.members.len(),
            management_locale_text("members", "个成员", "個成員"),
            management_route_strategy_label(row.route.strategy)
        );
        let select_id = id.clone();
        workspace_nav_row(
            SharedString::from(format!("management-global-route-row-{id}")),
            selected,
            ws_icon_tile("icons/vibex/git-branch.svg", px(30.0), None, cx),
            row.route.display_name.clone(),
            div()
                .min_w_0()
                .truncate()
                .child(subtitle)
                .into_any_element(),
            (!enabled).then(|| {
                Tag::color(ColorName::Neutral)
                    .xsmall()
                    .child(management_locale_text("Off", "已停用", "已停用"))
                    .into_any_element()
            }),
            cx.listener(move |this, _, _, cx| {
                this.select_workspace_route_group(select_id.clone(), cx)
            }),
            cx,
        )
    }
}

/// What the gateway's state is called and the colour its dot takes.
pub(super) fn workspace_gateway_state(
    status: &vibex_core::ModelGatewayStatus,
    cx: &App,
) -> (&'static str, Hsla) {
    match status {
        vibex_core::ModelGatewayStatus::Running => (
            management_locale_text("Running", "运行中", "執行中"),
            cx.theme().success,
        ),
        vibex_core::ModelGatewayStatus::Disabled => (
            management_locale_text("Off", "已关闭", "已關閉"),
            cx.theme().muted_foreground,
        ),
        vibex_core::ModelGatewayStatus::Starting => (
            management_locale_text("Starting…", "启动中…", "啟動中…"),
            cx.theme().info,
        ),
        vibex_core::ModelGatewayStatus::Degraded => (
            management_locale_text("Degraded", "已降级", "已降級"),
            cx.theme().warning,
        ),
        vibex_core::ModelGatewayStatus::Unavailable => (
            management_locale_text("Unavailable", "不可用", "無法使用"),
            cx.theme().warning,
        ),
    }
}

/// One navigable row of the left column.
///
/// Every row has the same three lanes — mark, two lines of text, one trailing
/// state — so names and states form columns down the whole list. Selection is
/// a filled plate; hover is a lighter wash of the same colour.
#[allow(clippy::too_many_arguments)]
fn workspace_nav_row(
    id: SharedString,
    selected: bool,
    mark: AnyElement,
    title: impl Into<SharedString>,
    subtitle: AnyElement,
    trailing: Option<AnyElement>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    h_flex()
        .id(id)
        .w_full()
        .min_w_0()
        .items_center()
        .gap_2p5()
        .px_2()
        .py_1p5()
        .rounded(cx.theme().radius_lg)
        .cursor_pointer()
        .when(selected, |row| row.bg(cx.theme().accent))
        .when(!selected, |row| {
            row.hover(|style| style.bg(cx.theme().primary.opacity(0.06)))
        })
        .child(mark)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_medium()
                        .child(title.into()),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(subtitle),
                ),
        )
        .children(trailing.map(|trailing| div().flex_none().text_xs().child(trailing)))
        .on_click(on_click)
        .into_any_element()
}

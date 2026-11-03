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
        if self.workspace_selecting() {
            self.reconcile_workspace_selection();
        }
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

        let mut list = v_flex().w_full().min_w_0().gap_1p5();
        list = list.child(self.render_workspace_gateway_row(cx));
        // While selecting, one strip above the lists owns every batch command,
        // so the same click acts on Providers and route groups together.
        let visible_provider_ids = providers
            .iter()
            .map(|row| row.profile.id.as_str().to_string())
            .collect::<Vec<_>>();
        let visible_route_ids = routes
            .iter()
            .map(|row| row.route.id.as_str().to_string())
            .collect::<Vec<_>>();
        if self.workspace_selecting() {
            list = list.child(self.render_workspace_batch_bar(
                &visible_provider_ids,
                &visible_route_ids,
                cx,
            ));
        }

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
                let unfiltered =
                    query.is_empty() && self.provider_list_filter == ProviderListFilter::All;
                list = list.child(if unfiltered {
                    self.render_workspace_sidebar_add_card(
                        "management-workspace-providers-empty",
                        management_locale_text("New Provider", "新建供应商", "新增供應商"),
                        cx.listener(|this, _, window, cx| {
                            this.open_provider_create_page(None, window, cx)
                        }),
                        cx,
                    )
                } else {
                    self.render_workspace_sidebar_empty(
                        management_locale_text(
                            "No Provider matches.",
                            "没有匹配的供应商。",
                            "沒有符合的供應商。",
                        ),
                        cx,
                    )
                });
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
                list = list.child(if query.is_empty() {
                    self.render_workspace_sidebar_add_card(
                        "management-workspace-routes-empty",
                        management_locale_text("New route group", "新建路由组", "新增路由組"),
                        cx.listener(|this, _, window, cx| {
                            this.open_route_group_page_editor(window, cx)
                        }),
                        cx,
                    )
                } else {
                    self.render_workspace_sidebar_empty(
                        management_locale_text(
                            "No route group matches.",
                            "没有匹配的路由组。",
                            "沒有符合的路由組。",
                        ),
                        cx,
                    )
                });
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
                            .overflow_y_scroll()
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
                // Reading the preview and showing what it found are two
                // steps: the menu reads, and the dialog is the reader's chance
                // to leave a record out. It used to write every candidate on
                // this one click.
                let import = {
                    let center = center.clone();
                    let agents = importable_agents.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        center
                            .update(cx, |this, cx| {
                                this.import_shared_providers(agents.clone(), None, cx);
                                this.present_native_import_picker(window, cx);
                            })
                            .ok();
                    }
                };
                menu.item(
                    PopupMenuItem::new(management_locale_text("Provider", "供应商", "供應商"))
                        .icon(Icon::default().path("icons/vibex/database.svg"))
                        .on_click(provider),
                )
                .item(
                    PopupMenuItem::new(management_locale_text("Route group", "路由组", "路由組"))
                        .icon(Icon::default().path("icons/vibex/directions.svg"))
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
            // Selecting is worth offering only once there is more than one
            // record to choose between; on an empty page it would be a mode
            // with nothing in it.
            .when(
                !self.providers.is_empty() || !self.model_routes.is_empty(),
                |bar| {
                    bar.child(
                        Button::new("management-provider-select")
                            .small()
                            .ghost()
                            .when(self.workspace_selecting(), |button| button.selected(true))
                            .icon(Icon::default().path("icons/vibex/list-checks.svg"))
                            .tooltip(management_locale_text(
                                "Select several",
                                "批量选择",
                                "批次選取",
                            ))
                            .accessibility_label(management_locale_text(
                                "Select several",
                                "批量选择",
                                "批次選取",
                            ))
                            .on_click(
                                cx.listener(|this, _, _, cx| this.toggle_workspace_selecting(cx)),
                            ),
                    )
                },
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
            .icon(Icon::default().path("icons/vibex/funnel.svg"))
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

    /// The card that stands in for a list with no records yet.
    ///
    /// It is the same dashed, card-shaped invitation the Agent detail's model
    /// Provider panel draws: a full row the whole of which opens the editor,
    /// rather than a paragraph that reads like a warning. It carries only the
    /// command's own name — the section heading above it already says which
    /// list the new record joins, so a second explanatory line would repeat
    /// that context instead of adding a decision.
    fn render_workspace_sidebar_add_card(
        &self,
        id: &'static str,
        label: &'static str,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let border_color = cx.theme().border.opacity(0.55);
        div()
            .id(id)
            .relative()
            .w_full()
            .min_w_0()
            .min_h(px(56.0))
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .p_2()
            .rounded(px(8.0))
            .bg(cx.theme().background.opacity(0.45))
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().primary.opacity(0.05)))
            .on_click(on_click)
            .child(
                Icon::new(IconName::Plus)
                    .size(px(16.0))
                    .text_color(cx.theme().muted_foreground.opacity(0.8)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_medium()
                    .text_color(cx.theme().muted_foreground.opacity(0.9))
                    .child(label),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        paint_empty_source_border(bounds, border_color, window)
                    },
                )
                .absolute()
                .size_full(),
            )
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
            None,
            workspace_card_mark(ws_icon_mark(
                "icons/vibex/server.svg",
                px(22.0),
                Some(cx.theme().primary),
                cx,
            )),
            gateway_display_name(&gateway.settings),
            Some(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(subtitle)
                    .into_any_element(),
            ),
            Some(ws_state_fact(state, color)),
            None,
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
        let favorite = self.provider_favorites.contains(id.as_str());
        // Counts are redundant with the section header and the detail page, but
        // a missing Key is the one state the card still has to call out.
        let subtitle = row.profile.credentials.is_empty().then(|| {
            div()
                .text_color(cx.theme().warning)
                .child(management_locale_text(
                    "Needs a Key",
                    "需要 Key",
                    "需要 Key",
                ))
                .into_any_element()
        });
        // The card trails only what changes how the record is read at a glance:
        // an off record says so, a favourite is marked. Health stays on the
        // detail header, where its verdict has room to be named; a bare dot on
        // every card was a second colour with no word beside it.
        let trailing = if enabled {
            favorite.then(|| {
                Icon::new(IconName::StarFill)
                    .size_3()
                    .text_color(cx.theme().warning)
                    .into_any_element()
            })
        } else {
            Some(
                Tag::color(ColorName::Neutral)
                    .xsmall()
                    .child(management_locale_text("Off", "已停用", "已停用"))
                    .into_any_element(),
            )
        };
        let selecting = self.workspace_selecting();
        let checked = selecting.then(|| self.workspace_provider_selected(id.as_str()));
        let select_id = id.clone();
        workspace_nav_row(
            SharedString::from(format!("management-global-provider-row-{id}")),
            selected,
            checked,
            div()
                .when(!enabled, |mark| mark.opacity(0.55))
                .child(workspace_card_mark(provider_logo_mark(
                    &row.profile,
                    px(22.0),
                    cx,
                )))
                .into_any_element(),
            row.profile.display_name.clone(),
            subtitle,
            trailing,
            workspace_agent_logos(
                &format!("management-provider-row-{id}"),
                &row.agents,
                |agent| self.workspace_agent_label(agent),
                cx,
            ),
            cx.listener(move |this, _, _, cx| {
                if this.workspace_selecting() {
                    this.toggle_workspace_provider_selected(select_id.clone(), cx);
                } else {
                    this.select_workspace_provider(select_id.clone(), cx);
                }
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
        let selecting = self.workspace_selecting();
        let checked = selecting.then(|| self.workspace_route_selected(id.as_str()));
        let select_id = id.clone();
        workspace_nav_row(
            SharedString::from(format!("management-global-route-row-{id}")),
            selected,
            checked,
            workspace_card_mark(ws_icon_mark(
                "icons/vibex/directions.svg",
                px(22.0),
                None,
                cx,
            )),
            row.route.display_name.clone(),
            None,
            (!enabled).then(|| {
                Tag::color(ColorName::Neutral)
                    .xsmall()
                    .child(management_locale_text("Off", "已停用", "已停用"))
                    .into_any_element()
            }),
            workspace_agent_logos(
                &format!("management-route-row-{id}"),
                &row.agents,
                |agent| self.workspace_agent_label(agent),
                cx,
            ),
            cx.listener(move |this, _, _, cx| {
                if this.workspace_selecting() {
                    this.toggle_workspace_route_selected(select_id.clone(), cx);
                } else {
                    this.select_workspace_route_group(select_id.clone(), cx);
                }
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

/// The leading slot of a card in the left column.
///
/// The slot keeps its width and height whether the mark is a filled brand logo
/// or a line icon, so names and card heights line up down the column. The mark
/// sits in it bare: the rounded plate belongs to an identity header, where it
/// anchors one page rather than outlining every row.
fn workspace_card_mark(mark: AnyElement) -> AnyElement {
    div()
        .flex_none()
        .size(px(MANAGEMENT_SIDEBAR_MARK_SLOT_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .child(mark)
        .into_any_element()
}

/// One card of the left column.
///
/// Each record is a bordered card: the mark leads, the name and its state fill
/// the middle, and the Agents that use the record trail the card as a row of
/// logos. Selection fills the card with the accent plate; hover is a lighter
/// wash, so pointing never outshines a click.
#[allow(clippy::too_many_arguments)]
fn workspace_nav_row(
    id: SharedString,
    selected: bool,
    checked: Option<bool>,
    mark: AnyElement,
    title: impl Into<SharedString>,
    subtitle: Option<AnyElement>,
    trailing: Option<AnyElement>,
    agent_logos: Option<AnyElement>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let check_id = id.clone();
    let card_style = management_sidebar_card_style(selected, cx);
    h_flex()
        .id(id)
        .w_full()
        .min_w_0()
        .items_center()
        .gap_1()
        .p_2()
        .rounded(px(8.0))
        .border_1()
        .border_color(card_style.border)
        .bg(card_style.background)
        .cursor_pointer()
        .hover(move |style| style.bg(card_style.hover_background))
        // While selecting, a checkbox leads the card: the click names a set in
        // this mode, so which records are in it has to be the leading fact.
        // The box stops the click from reaching the row, because the row's
        // handler would toggle the same record a second time.
        .when_some(checked, |row, checked| {
            row.child(
                div()
                    .id(SharedString::from(format!("{check_id}-check-wrap")))
                    .flex_none()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        Checkbox::new(SharedString::from(format!("{check_id}-check")))
                            .small()
                            .checked(checked),
                    ),
            )
        })
        .child(mark)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_medium()
                        .child(title.into()),
                )
                .children(subtitle.map(|subtitle| {
                    div()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(subtitle)
                })),
        )
        .children(agent_logos)
        .children(trailing.map(|trailing| div().flex_none().text_xs().child(trailing)))
        .on_click(on_click)
        .into_any_element()
}

/// The Agents that use a record, trailing the card as a row of logos.
///
/// The row keeps the card's right edge stable: it draws at most five marks and
/// summarises the rest as one count, so a record shared by every Agent never
/// pushes the name out of its column. It is shared with the MCP and Skill
/// sidebars, whose records carry the same per-Agent enablement.
pub(in crate::management) fn workspace_agent_logos(
    id: &str,
    agents: &BTreeSet<String>,
    labels: impl Fn(&str) -> String,
    cx: &App,
) -> Option<AnyElement> {
    const LIMIT: usize = 5;
    if agents.is_empty() {
        return None;
    }
    Some(
        h_flex()
            .flex_none()
            .items_center()
            .gap_1()
            .children(agents.iter().take(LIMIT).map(|agent_id| {
                let tooltip = SharedString::from(labels(agent_id));
                div()
                    .id(SharedString::from(format!("{id}-agent-{agent_id}")))
                    .flex_none()
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                    })
                    .child(agent_brand_icon(agent_id, px(14.0), None))
            }))
            .when(agents.len() > LIMIT, |stack| {
                // The count is the card's only clipped fact, so hovering it
                // names the Agents it stands for instead of leaving them
                // unreachable from the card.
                let rest = SharedString::from(
                    agents
                        .iter()
                        .skip(LIMIT)
                        .map(|agent_id| labels(agent_id))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                stack.child(
                    div()
                        .id(SharedString::from(format!("{id}-agent-rest")))
                        .flex_none()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(rest.clone()).build(window, cx)
                        })
                        .child(format!("+{}", agents.len() - LIMIT)),
                )
            })
            .into_any_element(),
    )
}

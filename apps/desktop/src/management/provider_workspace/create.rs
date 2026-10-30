//! The new-Provider page.
//!
//! Two steps. The first is a gallery: every built-in preset as a tile, with
//! its logo, grouped by kind and searchable, plus a blank "Custom" tile. The
//! second is a short form the preset has already filled: name, address and
//! Key up front, models next, and everything a preset rarely needs folded
//! under Advanced.

use super::*;

const CREATE_ADVANCED_DISCLOSURE_ID: &str = "management-provider-create-advanced";

impl ManagementCenter {
    pub(super) fn render_workspace_provider_create(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.workspace_create_choosing {
            self.render_workspace_create_gallery(cx)
        } else {
            self.render_workspace_create_form(cx)
        }
    }

    fn render_workspace_create_gallery(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.workspace_create_search.read(cx).value().to_string();
        let active = self.workspace_create_category;
        let presets = vibex_core::provider_presets()
            .into_iter()
            .filter(|preset| provider_preset_matches(preset, &query))
            .filter(|preset| {
                active.is_none_or(|category| provider_preset_category(preset) == category)
            })
            .collect::<Vec<_>>();

        let mut chips = h_flex().items_center().gap_1();
        for (index, (category, label)) in
            std::iter::once((None, management_locale_text("All", "全部", "全部")))
                .chain(
                    ProviderPresetCategory::ALL
                        .into_iter()
                        .map(|category| (Some(category), category.label())),
                )
                .enumerate()
        {
            chips = chips.child(
                Button::new(SharedString::from(format!(
                    "management-provider-create-kind-{index}"
                )))
                .small()
                .ghost()
                .selected(category == active)
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.workspace_create_category = category;
                    cx.notify();
                })),
            );
        }

        let mut grid = h_flex().w_full().min_w_0().flex_wrap().gap_3();
        // The blank tile leads: "none of these" is always one click away.
        grid = grid.child(create_tile(
            SharedString::from("management-provider-create-custom"),
            ws_icon_tile("icons/vibex/wrench.svg", px(36.0), None, cx),
            management_locale_text("Custom", "自定义", "自訂").to_string(),
            management_locale_text(
                "Any OpenAI- or Anthropic-compatible address",
                "任意兼容 OpenAI 或 Anthropic 的地址",
                "任意相容 OpenAI 或 Anthropic 的位址",
            )
            .to_string(),
            None,
            cx.listener(|this, _, window, cx| {
                this.clear_provider_create_preset(window, cx);
                this.workspace_create_choosing = false;
                cx.notify();
            }),
            cx,
        ));
        for preset in &presets {
            let preset_id = preset.id.clone();
            let host = preset
                .website
                .as_deref()
                .map(provider_preset_host)
                .or_else(|| {
                    preset
                        .endpoints
                        .values()
                        .next()
                        .map(|url| provider_preset_host(url))
                })
                .unwrap_or_default();
            let note = if preset.no_key {
                Some(management_locale_text("No Key", "无需 Key", "無需 Key"))
            } else if preset.plan_note.is_some() {
                Some(management_locale_text("Plan", "套餐", "方案"))
            } else {
                None
            };
            grid = grid.child(create_tile(
                SharedString::from(format!("management-provider-create-preset-{}", preset.id)),
                preset_logo_tile(preset, px(36.0), cx),
                preset.display_name.clone(),
                host,
                note,
                cx.listener(move |this, _, window, cx| {
                    this.apply_provider_create_preset(&preset_id, window, cx);
                    this.workspace_create_choosing = false;
                    cx.notify();
                }),
                cx,
            ));
        }

        let back = Button::new("management-provider-create-close")
            .small()
            .ghost()
            .icon(IconName::ArrowLeft)
            .label(management_locale_text("Back", "返回", "返回"))
            .on_click(cx.listener(|this, _, _, cx| this.close_provider_create_page(cx)))
            .into_any_element();
        ws_authoring_page(
            "management-provider-create-page",
            back,
            None,
            management_locale_text("New Provider", "新建供应商", "新增供應商"),
            management_locale_text(
                "Pick where the models come from. A preset fills in the address and the model list for you.",
                "选择模型来源。选用预设会自动填好地址和模型列表。",
                "選擇模型來源。選用預設會自動填好位址和模型清單。",
            ),
            v_flex()
                .w_full()
                .gap_4()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .flex_wrap()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w(rems(16.0))
                                .child(management_search_input(&self.workspace_create_search, cx)),
                        )
                        .child(chips),
                )
                .child(grid)
                .when(presets.is_empty(), |page| {
                    page.child(ws_empty_line(
                        management_locale_text(
                            "No preset matches. Start from Custom instead.",
                            "没有匹配的预设，可以从“自定义”开始。",
                            "沒有符合的預設，可以從「自訂」開始。",
                        ),
                        cx,
                    ))
                })
                .into_any_element(),
            cx,
        )
    }

    fn render_workspace_create_form(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
        let preset = self.provider_create_preset.clone();

        // Region, when the preset serves more than one.
        let region = preset
            .as_ref()
            .filter(|preset| !preset.regions.is_empty())
            .map(|preset| {
                let choices = std::iter::once((
                    None,
                    management_locale_text("Default", "默认", "預設").to_string(),
                ))
                .chain(
                    preset
                        .regions
                        .iter()
                        .map(|region| (Some(region.id.clone()), region.display_name.clone())),
                )
                .collect::<Vec<_>>();
                let current = self.provider_create_preset_region.clone();
                let label = choices
                    .iter()
                    .find(|(id, _)| *id == current)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_default();
                let center = cx.weak_entity();
                ws_field(
                    management_locale_text("Region", "区域", "區域"),
                    None,
                    Button::new("management-provider-create-region")
                        .outline()
                        .w_full()
                        .label(label)
                        .dropdown_caret(true)
                        .dropdown_menu(move |mut menu, _, _| {
                            for (id, label) in choices.clone() {
                                let center = center.clone();
                                let checked = id == current;
                                menu =
                                    menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                                        move |_, window, cx| {
                                            center
                                                .update(cx, |this, cx| {
                                                    this.apply_provider_create_preset_region(
                                                        id.clone(),
                                                        window,
                                                        cx,
                                                    )
                                                })
                                                .ok();
                                        },
                                    ));
                            }
                            menu
                        })
                        .into_any_element(),
                    cx,
                )
            });
        let key_hint = preset
            .as_ref()
            .and_then(|preset| preset.key_hint.clone())
            .map(SharedString::from);
        let keys_url = preset.as_ref().and_then(|preset| preset.keys_url.clone());
        let no_key = preset.as_ref().is_some_and(|preset| preset.no_key);

        let basics = ws_section(
            management_locale_text("Basics", "基本信息", "基本資訊"),
            None,
            None,
            v_flex()
                .w_full()
                .gap_4()
                .child(match region {
                    Some(region) => ws_field_pair(
                        ws_field(
                            management_locale_text("Name", "名称", "名稱"),
                            None,
                            ws_input(
                                &self.provider_create_name,
                                false,
                                self.provider_create_name_error.as_deref(),
                                cx,
                            ),
                            cx,
                        ),
                        region,
                    ),
                    None => ws_field(
                        management_locale_text("Name", "名称", "名稱"),
                        Some(management_locale_text(
                            "How the Provider is listed everywhere.",
                            "供应商在各处显示的名称。",
                            "供應商在各處顯示的名稱。",
                        )),
                        ws_input(
                            &self.provider_create_name,
                            false,
                            self.provider_create_name_error.as_deref(),
                            cx,
                        ),
                        cx,
                    ),
                })
                .child(ws_field(
                    management_locale_text("API address", "API 地址", "API 位址"),
                    Some(management_locale_text(
                        "The base URL requests are sent to.",
                        "请求发往的基础地址。",
                        "請求發往的基礎位址。",
                    )),
                    ws_input(
                        &self.provider_create_base_url,
                        false,
                        self.provider_create_base_url_error.as_deref(),
                        cx,
                    ),
                    cx,
                ))
                .when(!no_key, |form| {
                    form.child(
                        v_flex()
                            .w_full()
                            .gap_1p5()
                            .child(
                                h_flex()
                                    .w_full()
                                    .items_center()
                                    .justify_between()
                                    .child(div().text_sm().font_medium().child("API Key"))
                                    .children(keys_url.map(|url| {
                                        gpui_component::link::Link::new("management-provider-create-keys-url")
                                            .href(url)
                                            .child(div().text_xs().child(management_locale_text(
                                                "Get a Key",
                                                "获取 Key",
                                                "取得 Key",
                                            )))
                                    })),
                            )
                            .child(ws_input(&self.provider_create_api_key, true, None, cx))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(key_hint.unwrap_or_else(|| {
                                        SharedString::from(management_locale_text(
                                            "Stored once and shared by every Agent that uses this Provider. You can add more later.",
                                            "只保存一次，所有使用该供应商的 Agent 共用；之后还可以添加更多。",
                                            "只儲存一次，所有使用該供應商的 Agent 共用；之後還可以新增更多。",
                                        ))
                                    })),
                            ),
                    )
                })
                .into_any_element(),
            cx,
        );

        let models = ws_section(
            management_locale_text("Models", "模型", "模型"),
            Some(management_locale_text(
                "You can change these at any time on the Provider's Models tab.",
                "之后随时可以在供应商的“模型”标签页中修改。",
                "之後隨時可以在供應商的「模型」分頁中修改。",
            )),
            None,
            v_flex()
                .w_full()
                .gap_4()
                .child(ws_field(
                    management_locale_text("Models to turn on", "启用的模型", "啟用的模型"),
                    Some(management_locale_text(
                        "Model ids separated by commas. Leave empty to fetch the list after saving.",
                        "用逗号分隔的模型 ID；留空则保存后自动获取列表。",
                        "用逗號分隔的模型 ID；留空則儲存後自動取得清單。",
                    )),
                    ws_input(
                        &self.provider_create_models,
                        false,
                        self.provider_create_models_error.as_deref(),
                        cx,
                    ),
                    cx,
                ))
                .child(ws_field(
                    management_locale_text("Default model", "默认模型", "預設模型"),
                    None,
                    ws_input(&self.provider_create_default_model, false, None, cx),
                    cx,
                ))
                .into_any_element(),
            cx,
        );

        let open = self.provider_create_headers_open;
        let advanced_body = v_flex()
            .w_full()
            .gap_4()
            .pt_2()
            .child(ws_field(
                management_locale_text("Kind", "类型", "類型"),
                None,
                ws_choice_cards(
                    "management-provider-create-category",
                    vec![
                        ("vendor", SharedString::from(management_locale_text("Official", "官方", "官方")), None),
                        ("relay", SharedString::from(management_locale_text("Relay", "中转", "中轉")), None),
                        ("local", SharedString::from(management_locale_text("Local", "本机", "本機")), None),
                    ],
                    self.provider_create_category.as_deref(),
                    pending,
                    |this, value, cx| {
                        this.provider_create_category = Some(value.to_string());
                        cx.notify();
                    },
                    cx,
                ),
                cx,
            ))
            .child(ws_field_pair(
                ws_field(
                    management_locale_text("Model list address", "模型列表地址", "模型清單位址"),
                    None,
                    ws_input(&self.provider_create_models_url, false, None, cx),
                    cx,
                ),
                ws_field(
                    management_locale_text("Sign-in address", "认证地址", "認證位址"),
                    None,
                    ws_input(&self.provider_create_auth_url, false, None, cx),
                    cx,
                ),
            ))
            .child(ws_field(
                management_locale_text("Proxy", "代理", "代理"),
                Some(management_locale_text(
                    "Empty follows the system proxy.",
                    "留空则跟随系统代理。",
                    "留空則跟隨系統代理。",
                )),
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(self.provider_create_proxy_disabled, |field| field.opacity(0.5))
                            .child(ws_input(&self.provider_create_proxy_url, false, None, cx)),
                    )
                    .child(
                        Switch::new("management-provider-create-proxy-disabled")
                            .small()
                            .label(management_locale_text("Direct", "直连", "直連"))
                            .checked(self.provider_create_proxy_disabled)
                            .disabled(pending)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.provider_create_proxy_disabled = *checked;
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
                cx,
            ))
            .child(ws_field(
                management_locale_text("Request headers", "请求头", "請求標頭"),
                Some(management_locale_text(
                    "A JSON object of header names and values. Use {\"variable\": \"session_id\"} for the session header.",
                    "JSON 对象，键为请求头名称；会话请求头写作 {\"variable\": \"session_id\"}。",
                    "JSON 物件，鍵為請求標頭名稱；工作階段請求標頭寫作 {\"variable\": \"session_id\"}。",
                )),
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        Textarea::new(&self.provider_create_headers)
                            .small()
                            .w_full()
                            .h(px(92.0)),
                    )
                    .children(self.provider_create_endpoint_error.clone().map(|message| {
                        div().text_xs().text_color(cx.theme().danger).child(message)
                    }))
                    .into_any_element(),
                cx,
            ))
            .child(ws_field(
                management_locale_text("models.dev id", "models.dev ID", "models.dev ID"),
                Some(management_locale_text(
                    "Only powers suggestions; never changes a request.",
                    "仅用于提供建议，不会改变请求。",
                    "僅用於提供建議，不會改變請求。",
                )),
                ws_input(&self.provider_create_catalog_id, false, None, cx),
                cx,
            ))
            .child(management_setting_row(
                management_locale_text("Route groups only", "仅通过路由组使用", "僅透過路由組使用"),
                management_locale_text(
                    "Keep these models out of Agent pickers; route groups still reach them.",
                    "不在 Agent 的模型选择器中显示，路由组仍可调用。",
                    "不在 Agent 的模型選擇器中顯示，路由組仍可呼叫。",
                ),
                Switch::new("management-provider-create-only-groups")
                    .small()
                    .checked(self.provider_create_only_through_groups)
                    .disabled(pending)
                    .on_click(cx.listener(|this, checked, _, cx| {
                        this.provider_create_only_through_groups = *checked;
                        cx.notify();
                    }))
                    .into_any_element(),
                cx,
            ));
        let advanced = v_flex()
            .w_full()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .px_4()
            .py_2()
            .child(
                h_flex()
                    .id(CREATE_ADVANCED_DISCLOSURE_ID)
                    .w_full()
                    .items_center()
                    .gap_2()
                    .py_1()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.provider_create_headers_open = !this.provider_create_headers_open;
                        cx.notify();
                    }))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size_4()
                        .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .child(management_locale_text("Advanced", "高级", "進階")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(management_locale_text(
                                        "Kind, extra addresses, proxy, headers",
                                        "类型、其他地址、代理、请求头",
                                        "類型、其他位址、代理、請求標頭",
                                    )),
                            ),
                    ),
            )
            .child(
                Collapsible::new()
                    .open(open)
                    .motion_id(CREATE_ADVANCED_DISCLOSURE_ID)
                    .content(advanced_body),
            )
            .into_any_element();

        let footer = ws_form_footer(
            self.provider_create_error.clone(),
            vec![
                Button::new("management-provider-create-cancel")
                    .ghost()
                    .label(management_locale_text("Cancel", "取消", "取消"))
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _, cx| this.close_provider_create_page(cx)))
                    .into_any_element(),
                Button::new("management-provider-create-save")
                    .primary()
                    .label(management_locale_text(
                        "Create Provider",
                        "创建供应商",
                        "建立供應商",
                    ))
                    .loading(pending)
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _, cx| this.save_provider_create_page(cx)))
                    .into_any_element(),
            ],
            cx,
        );

        let back = Button::new("management-provider-create-close")
            .small()
            .ghost()
            .icon(IconName::ArrowLeft)
            .label(management_locale_text(
                "All presets",
                "全部预设",
                "全部預設",
            ))
            .disabled(pending)
            .on_click(cx.listener(|this, _, _, cx| {
                this.workspace_create_choosing = true;
                cx.notify();
            }))
            .into_any_element();
        let (mark, title) = match preset.as_ref() {
            Some(preset) => (
                preset_logo_tile(preset, px(44.0), cx),
                format!(
                    "{} {}",
                    management_locale_text("New Provider ·", "新建供应商 ·", "新增供應商 ·"),
                    preset.display_name
                ),
            ),
            None => (
                ws_icon_tile("icons/vibex/wrench.svg", px(44.0), None, cx),
                management_locale_text("New custom Provider", "新建自定义供应商", "新增自訂供應商")
                    .to_string(),
            ),
        };
        ws_authoring_page(
            "management-provider-create-page",
            back,
            Some(mark),
            title,
            management_locale_text(
                "A shared record: any Agent can use it once it exists.",
                "共享记录：创建后任何 Agent 都可以使用。",
                "共用記錄：建立後任何 Agent 都可以使用。",
            ),
            ws_stack(vec![basics, models, advanced, footer]),
            cx,
        )
    }
}

/// A preset's mark, looked up the same way a saved Provider's is.
fn preset_logo_tile(preset: &vibex_core::ProviderPreset, size: Pixels, cx: &App) -> AnyElement {
    let profile = vibex_core::ModelProviderProfile {
        display_name: preset.display_name.clone(),
        vendor_hint: Some(preset.id.clone()),
        metadata: vibex_core::ModelProviderMetadata {
            catalog_id: preset.catalog_id.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    provider_logo_tile(&profile, size, cx)
}

/// One gallery tile.
fn create_tile(
    id: SharedString,
    mark: AnyElement,
    title: String,
    subtitle: String,
    note: Option<&'static str>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    h_flex()
        .id(id)
        .flex_1()
        .min_w(rems(13.0))
        .max_w(rems(19.0))
        .items_center()
        .gap_3()
        .p_3()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(cx.theme().border)
        .bg(theme::semantic_color("card", cx.theme().is_dark()).opacity(0.72))
        .cursor_pointer()
        .hover(|style| {
            style
                .border_color(cx.theme().primary.opacity(0.6))
                .bg(cx.theme().primary.opacity(0.05))
        })
        .child(mark)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .min_w_0()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .font_medium()
                                .child(title),
                        )
                        .children(
                            note.map(|note| Tag::color(ColorName::Neutral).xsmall().child(note)),
                        ),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(subtitle),
                ),
        )
        .on_click(on_click)
        .into_any_element()
}

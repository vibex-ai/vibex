//! The one Provider form, and the scope that decides which record it writes.
//!
//! The Providers page used to answer "new Provider" and "edit a Provider" with
//! two different forms and two different drafts. They drifted: note and website
//! only existed on one of them, per-protocol addresses only on the other, and
//! the route-groups-only switch had two writers of which the form always wrote
//! the stale one back. The page now has one draft and one body; a scope says
//! whether it is creating a record or replacing an existing one.

use super::*;

/// Which record the open Provider form is describing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::management) enum ProviderEditorScope {
    /// A new shared record. There is no id and no revision yet.
    Create,
    /// The saved record under the Settings tab, at the revision it was loaded.
    Shared {
        id: vibex_core::ModelProviderProfileId,
        revision: i64,
    },
}

impl ProviderEditorScope {
    pub(in crate::management) fn is_create(&self) -> bool {
        matches!(self, Self::Create)
    }
}

/// The Provider form's draft.
///
/// Every field the form can write lives here so the same body can serve both
/// scopes. Fields a scope cannot use stay empty rather than being mirrored into
/// a second struct: the API Key is only collected while creating (a saved
/// Provider adds Keys on its own tab), and the enabled-model list is only a
/// text field while creating (a saved Provider edits models on its Models tab).
pub(in crate::management) struct ProviderDraft {
    pub(in crate::management) scope: ProviderEditorScope,
    /// Which record the loaded values belong to, so a reload is detected even
    /// when the scope changes for the same id.
    pub(in crate::management) loaded_revision: Option<i64>,
    pub(in crate::management) name: Entity<InputState>,
    pub(in crate::management) note: Entity<InputState>,
    pub(in crate::management) website_url: Entity<InputState>,
    pub(in crate::management) base_url: Entity<InputState>,
    /// One address per protocol. The request address above stays the
    /// provider-wide fallback a protocol without an override uses.
    pub(in crate::management) protocol_base_urls:
        Vec<(vibex_core::ProviderModelWireApi, Entity<InputState>)>,
    pub(in crate::management) protocol_open: bool,
    pub(in crate::management) api_key: Entity<InputState>,
    pub(in crate::management) models: Entity<InputState>,
    pub(in crate::management) default_model: Entity<InputState>,
    pub(in crate::management) models_url: Entity<InputState>,
    pub(in crate::management) auth_url: Entity<InputState>,
    pub(in crate::management) proxy_endpoint_url: Entity<InputState>,
    pub(in crate::management) proxy_url: Entity<InputState>,
    pub(in crate::management) proxy_disabled: bool,
    pub(in crate::management) headers: Entity<TextareaState>,
    pub(in crate::management) headers_open: bool,
    pub(in crate::management) catalog_id: Entity<InputState>,
    pub(in crate::management) icon: Entity<InputState>,
    pub(in crate::management) category: Option<String>,
    pub(in crate::management) only_through_groups: bool,
    pub(in crate::management) error: Option<String>,
    pub(in crate::management) name_error: Option<String>,
    pub(in crate::management) base_url_error: Option<String>,
    pub(in crate::management) models_error: Option<String>,
    pub(in crate::management) endpoint_error: Option<String>,
}

impl ProviderDraft {
    pub(in crate::management) fn new(
        window: &mut Window,
        cx: &mut Context<ManagementCenter>,
    ) -> Self {
        Self {
            scope: ProviderEditorScope::Create,
            loaded_revision: None,
            name: cx.new(|cx| {
                InputState::new(window, cx).placeholder(management_locale_text(
                    "Provider name",
                    "供应商名称",
                    "供應商名稱",
                ))
            }),
            note: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(management_locale_text("Note", "备注", "備註"))
            }),
            website_url: cx
                .new(|cx| InputState::new(window, cx).placeholder("https://provider.example")),
            base_url: cx
                .new(|cx| InputState::new(window, cx).placeholder("https://api.example.com/v1")),
            protocol_base_urls: Vec::new(),
            protocol_open: false,
            api_key: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(PROVIDER_API_KEY_PLACEHOLDER)
                    .masked(true)
            }),
            models: cx.new(|cx| {
                InputState::new(window, cx).placeholder(management_locale_text(
                    "model-a, model-b",
                    "模型 A, 模型 B",
                    "模型 A, 模型 B",
                ))
            }),
            default_model: cx.new(|cx| {
                InputState::new(window, cx).placeholder(management_locale_text(
                    "default model",
                    "默认模型",
                    "預設模型",
                ))
            }),
            models_url: cx.new(|cx| {
                InputState::new(window, cx).placeholder(management_locale_text(
                    "models endpoint",
                    "模型列表地址",
                    "模型清單位址",
                ))
            }),
            auth_url: cx.new(|cx| {
                InputState::new(window, cx).placeholder(management_locale_text(
                    "auth endpoint",
                    "认证地址",
                    "認證位址",
                ))
            }),
            proxy_endpoint_url: cx.new(|cx| {
                InputState::new(window, cx).placeholder(management_locale_text(
                    "proxy service",
                    "代理服务地址",
                    "代理服務位址",
                ))
            }),
            proxy_url: cx
                .new(|cx| InputState::new(window, cx).placeholder("http://127.0.0.1:7890")),
            proxy_disabled: false,
            headers: cx.new(|cx| {
                // The draft opens on the empty object rather than a blank
                // textarea: `{}` is what the field parses on save, and an
                // empty field only reads as valid after the user guesses that
                // shape. A preset or a saved record overwrites it when it has
                // headers of its own.
                TextareaState::new(window, cx)
                    .default_value(PROVIDER_HEADERS_EMPTY_OBJECT)
                    .placeholder(management_locale_text(
                        "{\n  \"X-Header\": \"value\"\n}",
                        "{\n  \"X-Header\": \"值\"\n}",
                        "{\n  \"X-Header\": \"值\"\n}",
                    ))
            }),
            headers_open: false,
            catalog_id: cx.new(|cx| {
                InputState::new(window, cx).placeholder(management_locale_text(
                    "models.dev id",
                    "models.dev ID",
                    "models.dev ID",
                ))
            }),
            icon: cx.new(|cx| {
                InputState::new(window, cx).placeholder(management_locale_text(
                    "icon key",
                    "图标 key",
                    "圖示 key",
                ))
            }),
            category: None,
            only_through_groups: false,
            error: None,
            name_error: None,
            base_url_error: None,
            models_error: None,
            endpoint_error: None,
        }
    }

    /// Empties every field and its messages, for a fresh create.
    pub(in crate::management) fn reset(
        &mut self,
        window: &mut Window,
        cx: &mut Context<ManagementCenter>,
    ) {
        self.scope = ProviderEditorScope::Create;
        self.loaded_revision = None;
        for input in [
            &self.name,
            &self.note,
            &self.website_url,
            &self.base_url,
            &self.models,
            &self.default_model,
            &self.models_url,
            &self.auth_url,
            &self.proxy_endpoint_url,
            &self.proxy_url,
            &self.catalog_id,
            &self.icon,
        ]
        .into_iter()
        .chain(self.protocol_base_urls.iter().map(|(_, input)| input))
        {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.api_key.update(cx, |state, cx| {
            state.set_masked(true, window, cx);
            state.set_placeholder(PROVIDER_API_KEY_PLACEHOLDER, window, cx);
            state.set_value("", window, cx);
        });
        self.headers.update(cx, |state, cx| {
            state.set_value(PROVIDER_HEADERS_EMPTY_OBJECT, window, cx)
        });
        self.protocol_open = false;
        self.proxy_disabled = false;
        self.headers_open = false;
        self.category = None;
        self.only_through_groups = false;
        self.error = None;
        self.name_error = None;
        self.base_url_error = None;
        self.models_error = None;
        self.endpoint_error = None;
    }

    /// Rebuilds the per-protocol address fields for the protocols a record
    /// declares, keeping any value already entered for one of them.
    ///
    /// A record may publish several protocols, and a protocol it does not
    /// publish yet still has to be enterable: the field set is therefore every
    /// protocol the editor offers, not only the saved ones.
    pub(in crate::management) fn rebuild_protocol_fields(
        &mut self,
        values: &BTreeMap<String, String>,
        window: &mut Window,
        cx: &mut Context<ManagementCenter>,
    ) {
        self.protocol_base_urls.clear();
        for wire_api in provider_form_wire_apis() {
            let key = wire_api.protocol_base_url_option_key();
            let value = values.get(&key).cloned().unwrap_or_default();
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value)
                    .placeholder("https://provider.example")
            });
            self.protocol_base_urls.push((wire_api, input));
        }
    }

    /// The per-protocol addresses the draft holds, as endpoint records.
    pub(in crate::management) fn protocol_endpoints(
        &self,
        cx: &App,
    ) -> Vec<vibex_core::ModelProviderEndpoint> {
        self.protocol_base_urls
            .iter()
            .filter_map(|(wire_api, input)| {
                let url = input.read(cx).value().trim().to_string();
                if url.is_empty() {
                    return None;
                }
                Some(vibex_core::ModelProviderEndpoint {
                    id: format!("api-{}", wire_api.wire_protocol_id().replace('_', "-")),
                    kind: vibex_core::ModelProviderEndpointKind::Api,
                    url,
                    wire_protocol_id: Some(wire_api.wire_protocol_id().to_string()),
                })
            })
            .collect()
    }

    pub(in crate::management) fn protocol_override_count(&self, cx: &App) -> usize {
        self.protocol_base_urls
            .iter()
            .filter(|(_, input)| !input.read(cx).value().trim().is_empty())
            .count()
    }
}

/// The protocols the shared record can hold an address for.
///
/// A shared Provider is not narrowed to one Agent's interfaces, so every
/// protocol the runtime knows is offered rather than the selected Agent's
/// subset.
fn provider_form_wire_apis() -> Vec<vibex_core::ProviderModelWireApi> {
    vibex_core::ProviderModelWireApi::ALL.to_vec()
}

impl ManagementCenter {
    /// The Provider form: identity, connection and metadata, for either scope.
    ///
    /// The same body draws a new record and the saved record under Settings.
    /// Only the parts that genuinely cannot apply to one scope are conditional:
    /// the Key field is create-only, and so is the enabled-model text field.
    pub(super) fn render_provider_form(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.provider_form.scope.clone();
        let creating = scope.is_create();
        let pending = self.mutation.is_some();
        let preset = creating
            .then(|| self.provider_create_preset.clone())
            .flatten();
        let no_key = preset.as_ref().is_some_and(|preset| preset.no_key);
        let key_hint = preset
            .as_ref()
            .and_then(|preset| preset.key_hint.clone())
            .map(SharedString::from);
        let keys_url = preset.as_ref().and_then(|preset| preset.keys_url.clone());
        let category = self.provider_form.category.clone();
        let proxy_disabled = self.provider_form.proxy_disabled;
        let protocol_open = self.provider_form.protocol_open;
        let override_count = self.provider_form.protocol_override_count(cx);
        let headers_open = self.provider_form.headers_open;

        // An existing record's first API endpoint is the fallback the address
        // field edits; a create starts empty.
        let identity = v_flex()
            .w_full()
            .gap_4()
            .child(ws_field_pair(
                ws_field(
                    management_locale_text("Name", "名称", "名稱"),
                    Some(management_locale_text(
                        "How the Provider is listed everywhere.",
                        "供应商在各处显示的名称。",
                        "供應商在各處顯示的名稱。",
                    )),
                    ws_input(
                        &self.provider_form.name,
                        false,
                        self.provider_form.name_error.as_deref(),
                        cx,
                    ),
                    cx,
                ),
                ws_field(
                    management_locale_text("Note", "备注", "備註"),
                    Some(management_locale_text(
                        "A reminder for yourself; never sent anywhere.",
                        "只给自己看的备注，不会发送到任何地方。",
                        "只給自己看的備註，不會傳送到任何地方。",
                    )),
                    ws_input(&self.provider_form.note, false, None, cx),
                    cx,
                ),
            ))
            .child(ws_field(
                management_locale_text("Website URL", "官网链接", "官網連結"),
                Some(management_locale_text(
                    "The vendor's own page, for reference.",
                    "厂商自己的页面，仅供查阅。",
                    "廠商自己的頁面，僅供查閱。",
                )),
                ws_input(&self.provider_form.website_url, false, None, cx),
                cx,
            ))
            .child(ws_field(
                management_locale_text("Kind", "类型", "類型"),
                None,
                ws_choice_cards(
                    "management-provider-form-category",
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
                        this.provider_form.category = Some(value.to_string());
                        cx.notify();
                    },
                    cx,
                ),
                cx,
            ));

        let key_field = (creating && !no_key).then(|| {
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
                            gpui_component::link::Link::new("management-provider-form-keys-url")
                                .href(url)
                                .child(div().text_xs().child(management_locale_text(
                                    "Get a Key",
                                    "获取 Key",
                                    "取得 Key",
                                )))
                        })),
                )
                .child(ws_input(&self.provider_form.api_key, true, None, cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(key_hint.clone().unwrap_or_else(|| {
                            SharedString::from(management_locale_text(
                                "Stored once and shared by every Agent that uses this Provider. You can add more later.",
                                "只保存一次，所有使用该供应商的 Agent 共用；之后还可以添加更多。",
                                "只儲存一次，所有使用該供應商的 Agent 共用；之後還可以新增更多。",
                            ))
                        })),
                )
                .into_any_element()
        });

        let mut protocol_fields = v_flex().w_full().gap_2p5();
        for (wire_api, input) in &self.provider_form.protocol_base_urls {
            protocol_fields = protocol_fields.child(ws_field(
                provider_form_protocol_label(*wire_api),
                None,
                ws_input(input, false, None, cx),
                cx,
            ));
        }

        let connection = v_flex()
            .w_full()
            .gap_4()
            .children(key_field)
            .child(ws_field(
                management_locale_text("API address", "API 地址", "API 位址"),
                Some(management_locale_text(
                    "The base URL requests are sent to. Protocols with their own address below use that one instead.",
                    "请求发往的基础地址。下方单独设置了地址的协议会改用那个地址。",
                    "請求發往的基礎位址。下方單獨設定了位址的協定會改用那個位址。",
                )),
                ws_input(
                    &self.provider_form.base_url,
                    false,
                    self.provider_form.base_url_error.as_deref(),
                    cx,
                ),
                cx,
            ))
            .child(
                v_flex()
                    .w_full()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .px_4()
                    .py_2()
                    .child(
                        h_flex()
                            .id("management-provider-form-protocol-disclosure")
                            .w_full()
                            .items_center()
                            .gap_2()
                            .py_1()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.provider_form.protocol_open = !this.provider_form.protocol_open;
                                cx.notify();
                            }))
                            .child(
                                Icon::new(if protocol_open {
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
                                    .child(div().text_sm().font_medium().child(
                                        management_locale_text(
                                            "Set an address per protocol",
                                            "按协议分别设置地址",
                                            "按協定分別設定位址",
                                        ),
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(management_locale_text(
                                                "One Agent may speak Messages and another Chat; each protocol gets its own address.",
                                                "一个 Agent 可能说 Messages，另一个说 Chat；每种协议各用各的地址。",
                                                "一個 Agent 可能說 Messages，另一個說 Chat；每種協定各用各的位址。",
                                            )),
                                    ),
                            )
                            .when(override_count > 0, |row| {
                                row.child(Tag::color(ColorName::Neutral).xsmall().child(
                                    management_locale_text("set", "已设置", "已設定").to_string()
                                        + &format!(" {override_count}"),
                                ))
                            }),
                    )
                    .child(
                        Collapsible::new()
                            .open(protocol_open)
                            .motion_id("management-provider-form-protocol-disclosure")
                            .content(protocol_fields),
                    ),
            );

        let mut models_block = None;
        if creating {
            models_block = Some(ws_field_pair(
                ws_field(
                    management_locale_text("Models to turn on", "启用的模型", "啟用的模型"),
                    Some(if no_key {
                        management_locale_text(
                            "Model ids separated by commas. Fetch fills it from the address above; each model's settings live on the Models tab after saving.",
                            "用逗号分隔的模型 ID。“拉取模型”会按上方地址自动填入；保存后可在“模型”标签页逐个设置。",
                            "用逗號分隔的模型 ID。「擷取模型」會依上方位址自動填入；儲存後可在「模型」分頁逐一設定。",
                        )
                    } else {
                        management_locale_text(
                            "Model ids separated by commas. Fetch fills it from the address and Key above; each model's settings live on the Models tab after saving.",
                            "用逗号分隔的模型 ID。“拉取模型”会按上方地址与 Key 自动填入；保存后可在“模型”标签页逐个设置。",
                            "用逗號分隔的模型 ID。「擷取模型」會依上方位址與 Key 自動填入；儲存後可在「模型」分頁逐一設定。",
                        )
                    }),
                    h_flex()
                        .w_full()
                        .items_start()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(ws_input(
                            &self.provider_form.models,
                            false,
                            self.provider_form.models_error.as_deref(),
                            cx,
                        )))
                        .child(
                            Button::new("management-provider-form-fetch-models")
                                .outline()
                                .icon(Icon::default().path("icons/vibex/rotate-ccw.svg"))
                                .label(management_fetch_models_label())
                                .loading(matches!(
                                    &self.mutation,
                                    Some(ManagementMutation::ProviderProbe(action))
                                        if action == "create-page-models"
                                ))
                                .disabled(pending)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.fetch_provider_create_models(window, cx)
                                })),
                        )
                        .into_any_element(),
                    cx,
                ),
                ws_field(
                    management_locale_text("Default model", "默认模型", "預設模型"),
                    Some(management_locale_text(
                        "One of the ids above.",
                        "填上方其中一个模型 ID。",
                        "填上方其中一個模型 ID。",
                    )),
                    ws_input(&self.provider_form.default_model, false, None, cx),
                    cx,
                ),
            ));
        }

        let request = v_flex()
            .w_full()
            .gap_4()
            .children(models_block)
            .child(ws_field_pair(
                ws_field(
                    management_locale_text("Model list address", "模型列表地址", "模型清單位址"),
                    Some(management_locale_text(
                        "Leave empty to use the API address.",
                        "留空则使用 API 地址。",
                        "留空則使用 API 位址。",
                    )),
                    ws_input(&self.provider_form.models_url, false, None, cx),
                    cx,
                ),
                ws_field(
                    management_locale_text("Sign-in address", "认证地址", "認證位址"),
                    None,
                    ws_input(&self.provider_form.auth_url, false, None, cx),
                    cx,
                ),
            ))
            .child(ws_field_pair(
                ws_field(
                    management_locale_text("Proxy", "代理", "代理"),
                    Some(if proxy_disabled {
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
                                .when(proxy_disabled, |field| field.opacity(0.5))
                                .child(ws_input(&self.provider_form.proxy_url, false, None, cx)),
                        )
                        .child(
                            Switch::new("management-provider-form-proxy-disabled")
                                .small()
                                .label(management_locale_text("Direct", "直连", "直連"))
                                .checked(proxy_disabled)
                                .disabled(pending)
                                .on_click(cx.listener(|this, checked, _, cx| {
                                    this.provider_form.proxy_disabled = *checked;
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
                    ws_input(&self.provider_form.proxy_endpoint_url, false, None, cx),
                    cx,
                ),
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
                    .id("management-provider-form-advanced-disclosure")
                    .w_full()
                    .items_center()
                    .gap_2()
                    .py_1()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.provider_form.headers_open = !this.provider_form.headers_open;
                        cx.notify();
                    }))
                    .child(
                        Icon::new(if headers_open {
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
                                        "Headers, catalogue id, icon, visibility",
                                        "请求头、目录 ID、图标、可见性",
                                        "請求標頭、目錄 ID、圖示、可見性",
                                    )),
                            ),
                    ),
            )
            .child(
                Collapsible::new()
                    .open(headers_open)
                    .motion_id("management-provider-form-advanced-disclosure")
                    .content(
                        v_flex()
                            .w_full()
                            .gap_4()
                            .pt_2()
                            .child(ws_field(
                                management_locale_text("Request headers", "请求头", "請求標頭"),
                                Some(management_locale_text(
                                    "A JSON object of header names and values. Use {\"variable\": \"session_id\"} for the session header. Secret headers are kept but not shown.",
                                    "JSON 对象，键为请求头名称。会话请求头写作 {\"variable\": \"session_id\"}。Secret 请求头会保留但不显示。",
                                    "JSON 物件，鍵為請求標頭名稱。工作階段請求標頭寫作 {\"variable\": \"session_id\"}。Secret 請求標頭會保留但不顯示。",
                                )),
                                v_flex()
                                    .w_full()
                                    .gap_1()
                                    .child(Textarea::new(&self.provider_form.headers).small().w_full().h(px(92.0)))
                                    .children(self.provider_form.endpoint_error.clone().map(|message| {
                                        div().text_xs().text_color(cx.theme().danger).child(message)
                                    }))
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
                                    ws_input(&self.provider_form.catalog_id, false, None, cx),
                                    cx,
                                ),
                                ws_field(
                                    management_locale_text("Icon", "图标", "圖示"),
                                    Some(management_locale_text(
                                        "A bundled icon key, such as openai.",
                                        "内置图标的 key，例如 openai。",
                                        "內建圖示的 key，例如 openai。",
                                    )),
                                    ws_input(&self.provider_form.icon, false, None, cx),
                                    cx,
                                ),
                            ))
                            .child(management_setting_row(
                                management_locale_text(
                                    "Route groups only",
                                    "仅通过路由组使用",
                                    "僅透過路由組使用",
                                ),
                                management_locale_text(
                                    "Keep these models out of Agent pickers; route groups still reach them.",
                                    "不在 Agent 的模型选择器中显示，路由组仍可调用。",
                                    "不在 Agent 的模型選擇器中顯示，路由組仍可呼叫。",
                                ),
                                Switch::new("management-provider-form-only-groups")
                                    .small()
                                    .checked(self.provider_form.only_through_groups)
                                    .disabled(pending)
                                    .on_click(cx.listener(|this, checked, _, cx| {
                                        this.provider_form.only_through_groups = *checked;
                                        cx.notify();
                                    }))
                                    .into_any_element(),
                                cx,
                            )),
                    ),
            );

        ws_stack(vec![
            ws_section(
                management_locale_text("Basics", "基本信息", "基本資訊"),
                None,
                None,
                identity.into_any_element(),
                cx,
            ),
            ws_section(
                management_locale_text("Connection", "连接", "連線"),
                Some(management_locale_text(
                    "Applies to every Agent that uses this Provider once saved.",
                    "保存后对所有使用该供应商的 Agent 生效。",
                    "儲存後對所有使用該供應商的 Agent 生效。",
                )),
                None,
                connection.into_any_element(),
                cx,
            ),
            ws_section(
                management_locale_text("Request", "请求", "請求"),
                None,
                None,
                request.into_any_element(),
                cx,
            ),
            advanced.into_any_element(),
        ])
    }

    /// Writes the form, whichever record its scope names.
    pub(super) fn save_provider_form(&mut self, cx: &mut Context<Self>) {
        match self.provider_form.scope.clone() {
            ProviderEditorScope::Create => self.save_provider_create_page(cx),
            ProviderEditorScope::Shared { id, revision } => {
                self.save_shared_provider_form(id, revision, cx)
            }
        }
    }
}

fn provider_form_protocol_label(wire_api: vibex_core::ProviderModelWireApi) -> &'static str {
    match wire_api {
        vibex_core::ProviderModelWireApi::OpenaiChatCompletions => management_locale_text(
            "Chat Completions address",
            "Chat Completions 地址",
            "Chat Completions 位址",
        ),
        vibex_core::ProviderModelWireApi::OpenaiResponses => management_locale_text(
            "OpenAI Responses address",
            "OpenAI Responses 地址",
            "OpenAI Responses 位址",
        ),
        vibex_core::ProviderModelWireApi::AnthropicMessages => management_locale_text(
            "Anthropic Messages address",
            "Anthropic Messages 地址",
            "Anthropic Messages 位址",
        ),
        vibex_core::ProviderModelWireApi::MistralConversations => management_locale_text(
            "Mistral Conversations address",
            "Mistral Conversations 地址",
            "Mistral Conversations 位址",
        ),
        vibex_core::ProviderModelWireApi::GoogleGenerativeAi => {
            management_locale_text("Gemini address", "Gemini 地址", "Gemini 位址")
        }
        vibex_core::ProviderModelWireApi::GoogleVertex => {
            management_locale_text("Vertex address", "Vertex 地址", "Vertex 位址")
        }
        vibex_core::ProviderModelWireApi::AwsBedrockConverse => {
            management_locale_text("Bedrock address", "Bedrock 地址", "Bedrock 位址")
        }
    }
}

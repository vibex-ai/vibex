//! The one Model editor, shared by the Agent dialog and the Provider's Models tab.
//!
//! Both entries edit the same fields — enabled, id, display name, protocol,
//! reasoning, thinking levels, capability flags, context and output limits —
//! but each used to draw its own copy of them. The two copies drifted the
//! moment either side gained a control. The body now lives here; a caller
//! supplies the two things that genuinely differ: which protocols it may offer
//! (an Agent only speaks its own), and what saving means (the Agent dialog
//! writes through, the Models tab writes the record).
//!
//! The draft is deliberately plain data plus text inputs: it converts to and
//! from a configured model in one place, so neither caller re-implements the
//! "empty field means inherit" rules.

use super::*;

/// How one host renders the editor.
pub(in crate::management) struct ModelEditorConfig<'a> {
    /// Prefix every element id with this, so two hosts cannot collide.
    pub(in crate::management) id_prefix: &'a str,
    /// The protocols the host may offer, with the support note it can say
    /// about each. A blocked choice stays listed so the reader can see what
    /// the host cannot call, rather than the list silently shrinking.
    pub(in crate::management) wire_choices: Vec<WireChoice>,
    /// Whether the id can be retyped. A saved Model is addressed by id when it
    /// is edited inline, so its row cannot rename it underneath itself.
    pub(in crate::management) editable_id: bool,
    /// Overrides an Agent-owned catalogue: the id becomes a closed choice.
    pub(in crate::management) catalog_choices:
        Vec<(String, Option<vibex_core::ProviderModelWireApi>)>,
    pub(in crate::management) pending: bool,
    /// Whether the enabled switch leads the panel.
    pub(in crate::management) shows_enabled: bool,
    /// Whether an edit is the value immediately. The Agent dialog has no Apply
    /// step, so it writes through; a saved record waits for its Save button and
    /// only needs a repaint.
    pub(in crate::management) write_through: bool,
}

/// One Model's editable settings.
///
/// The values start from a configured model and are read back through
/// [`Self::to_capabilities`]; an empty field is the "inherit" answer, never a
/// zero.
pub(in crate::management) struct ModelEditDraft {
    pub(in crate::management) id: Entity<InputState>,
    pub(in crate::management) display_name: Entity<InputState>,
    pub(in crate::management) efforts: Entity<InputState>,
    pub(in crate::management) context_tokens: Entity<InputState>,
    pub(in crate::management) output_tokens: Entity<InputState>,
    pub(in crate::management) wire_api: Option<vibex_core::ProviderModelWireApi>,
    pub(in crate::management) reasoning_disabled: bool,
    pub(in crate::management) image_input: Option<bool>,
    pub(in crate::management) pdf_input: Option<bool>,
    pub(in crate::management) temperature: Option<bool>,
    pub(in crate::management) reasoning: Option<bool>,
    pub(in crate::management) enabled: bool,
    /// Whether a field edit is the value immediately.
    ///
    /// The Agent dialog has no Apply step, so its pane writes each keystroke
    /// into the dialog's draft. The Models tab waits for its Save button and
    /// reads the fields directly, so the same editor must not write there.
    pub(in crate::management) write_through: bool,
    /// The model the loaded values describe, so the host can tell a reload from
    /// a repaint without keeping a second selection.
    pub(in crate::management) target_id: Option<String>,
    /// The last parsed limits, kept so a suggestion can be compared without
    /// an app context at hand.
    pub(in crate::management) context_tokens_cache: Option<String>,
    pub(in crate::management) output_tokens_cache: Option<String>,
    pub(in crate::management) error: Option<ModelEditError>,
    pub(in crate::management) error_message: Option<String>,
    pub(in crate::management) suggestion: Option<vibex_core::GatewayCapabilitySuggestion>,
    pub(in crate::management) suggestion_auto_filled: usize,
    /// The field subscriptions that write edits through, kept alive with the
    /// draft. Dropping them would silently turn the editor back into one that
    /// only writes through on its controls.
    _subscriptions: Vec<gpui::Subscription>,
}

/// One protocol the editor may offer, and how the host can support it.
pub(in crate::management) struct WireChoice {
    pub(in crate::management) api: vibex_core::ProviderModelWireApi,
    /// A note beside the label: which integration would carry it.
    pub(in crate::management) support: Option<SharedString>,
    /// Whether the host can actually call it.
    pub(in crate::management) supported: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::management) enum ModelEditError {
    Id,
    Reasoning,
    Limits,
}

impl ModelEditDraft {
    pub(in crate::management) fn new(
        window: &mut Window,
        cx: &mut Context<ManagementCenter>,
    ) -> Self {
        let mut text = |placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let mut draft = Self {
            id: text(management_locale_text("Model id", "模型 ID", "模型 ID")),
            display_name: text(management_locale_text(
                "Display name",
                "显示名称",
                "顯示名稱",
            )),
            efforts: text("off, low, high, max"),
            context_tokens: text(management_locale_text(
                "e.g. 1000000",
                "例如 1000000",
                "例如 1000000",
            )),
            output_tokens: text(management_locale_text(
                "e.g. 128000",
                "例如 128000",
                "例如 128000",
            )),
            wire_api: None,
            reasoning_disabled: false,
            image_input: None,
            pdf_input: None,
            temperature: None,
            reasoning: None,
            enabled: true,
            write_through: false,
            target_id: None,
            context_tokens_cache: None,
            output_tokens_cache: None,
            error: None,
            error_message: None,
            suggestion: None,
            suggestion_auto_filled: 0,
            _subscriptions: Vec::new(),
        };
        // The fields write themselves into the owning draft as they are typed.
        // A host that only repaints on a change (the Models tab) leaves its
        // values here for its Save button to read; the Agent dialog, which has
        // no Apply step, cannot wait for a control that happens to write
        // through, or a typed Model id would be dropped by the dialog's Save.
        let mut subscriptions = Vec::new();
        for input in [
            &draft.id,
            &draft.display_name,
            &draft.context_tokens,
            &draft.output_tokens,
        ] {
            subscriptions.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if !crate::management::management_input_changed(event) {
                    return;
                }
                let write_through = this.model_editor.write_through;
                this.model_editor_changed(write_through, cx);
            }));
        }
        subscriptions.push(
            cx.subscribe(&draft.efforts, |this, _, event: &InputEvent, cx| {
                if !crate::management::management_input_changed(event) {
                    return;
                }
                // A written table is the more specific answer, so typing one leaves
                // the non-reasoning choice behind.
                if !this.model_editor.efforts.read(cx).value().trim().is_empty() {
                    this.model_editor.reasoning_disabled = false;
                }
                let write_through = this.model_editor.write_through;
                this.model_editor_changed(write_through, cx);
            }),
        );
        draft._subscriptions = subscriptions;
        draft
    }

    /// Points the fields at one model.
    pub(in crate::management) fn load(
        &mut self,
        model: &vibex_core::ProviderConfiguredModel,
        window: &mut Window,
        cx: &mut Context<ManagementCenter>,
    ) {
        let context = model
            .capabilities
            .context_tokens
            .map(|value| value.to_string())
            .unwrap_or_default();
        let output = model
            .capabilities
            .output_tokens
            .map(|value| value.to_string())
            .unwrap_or_default();
        let name = model.display_name.clone().unwrap_or_default();
        let efforts = model
            .capabilities
            .reasoning_efforts
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        for (input, value) in [
            (&self.id, model.id.clone()),
            (&self.display_name, name),
            (&self.context_tokens, context),
            (&self.output_tokens, output),
            (&self.efforts, efforts),
        ] {
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        self.wire_api = model.wire_api;
        self.enabled = model.enabled;
        self.context_tokens_cache = model
            .capabilities
            .context_tokens
            .map(|value| value.to_string());
        self.output_tokens_cache = model
            .capabilities
            .output_tokens
            .map(|value| value.to_string());
        self.reasoning_disabled = model.capabilities.reasoning == Some(false);
        self.image_input = model.capabilities.image_input;
        self.pdf_input = model.capabilities.pdf_input;
        self.temperature = model.capabilities.temperature;
        self.reasoning = model.capabilities.reasoning;
        self.error = None;
    }

    pub(in crate::management) fn clear(
        &mut self,
        window: &mut Window,
        cx: &mut Context<ManagementCenter>,
    ) {
        for input in [
            &self.id,
            &self.display_name,
            &self.context_tokens,
            &self.output_tokens,
            &self.efforts,
        ] {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.wire_api = None;
        self.context_tokens_cache = None;
        self.output_tokens_cache = None;
        self.reasoning_disabled = false;
        self.image_input = None;
        self.pdf_input = None;
        self.temperature = None;
        self.reasoning = None;
        self.error = None;
        self.error_message = None;
        self.suggestion = None;
        self.suggestion_auto_filled = 0;
        self.target_id = None;
    }

    /// Turns the fields into the model's id, name and capabilities.
    ///
    /// The id is returned separately because a host may address the model by
    /// it; the "inherit" rules stay here so both hosts agree on them.
    pub(in crate::management) fn read(
        &mut self,
        cx: &Context<ManagementCenter>,
    ) -> Option<(
        String,
        Option<String>,
        vibex_core::ProviderModelCapabilities,
    )> {
        let id = self.id.read(cx).value().trim().to_string();
        if id.is_empty() {
            self.error = Some(ModelEditError::Id);
            return None;
        }
        let display_name = self.display_name.read(cx).value().trim().to_string();
        let efforts_text = self.efforts.read(cx).value().trim().to_string();
        let context_text = self.context_tokens.read(cx).value().trim().to_string();
        let output_text = self.output_tokens.read(cx).value().trim().to_string();

        let mut capabilities = vibex_core::ProviderModelCapabilities {
            image_input: self.image_input,
            pdf_input: self.pdf_input,
            temperature: self.temperature,
            reasoning: self.reasoning,
            ..Default::default()
        };
        if self.reasoning_disabled {
            capabilities.reasoning = Some(false);
            capabilities.reasoning_efforts = None;
        } else if !efforts_text.is_empty() {
            match vibex_core::ProviderModelReasoningEfforts::parse(&efforts_text) {
                Ok(parsed) => {
                    // Declaring levels is declaring the capability; leaving the
                    // field empty returns the model to the host's default.
                    capabilities.reasoning = Some(true);
                    capabilities.reasoning_efforts = parsed;
                }
                Err(reason) => {
                    self.error = Some(ModelEditError::Reasoning);
                    self.error_message = Some(declared_reasoning_efforts_error(&reason));
                    return None;
                }
            }
        }
        match declared_token_limits(&context_text, &output_text) {
            Ok((context, output)) => {
                capabilities.context_tokens = context;
                capabilities.output_tokens = output;
            }
            Err(reason) => {
                self.error = Some(ModelEditError::Limits);
                self.error_message = Some(declared_token_limit_error(&reason));
                return None;
            }
        }
        self.error = None;
        self.error_message = None;
        Some((
            id,
            (!display_name.is_empty()).then_some(display_name),
            capabilities,
        ))
    }

    /// Whether a field still equals what the catalogue suggested.
    fn matches_suggestion(&self, field: &str, suggested: &str) -> bool {
        let current = match field {
            "contextTokens" => self.context_tokens_cache.clone(),
            "outputTokens" => self.output_tokens_cache.clone(),
            "reasoning" => self.reasoning.map(|value| value.to_string()),
            "imageInput" => self.image_input.map(|value| value.to_string()),
            "pdfInput" => self.pdf_input.map(|value| value.to_string()),
            "temperature" => self.temperature.map(|value| value.to_string()),
            _ => return false,
        };
        current.as_deref() == Some(suggested)
    }

    /// Records which of the form's values still equal the catalogue's.
    pub(in crate::management) fn mark_provenance(
        &mut self,
        capabilities: &mut vibex_core::ProviderModelCapabilities,
    ) {
        let Some(suggestion) = self.suggestion.as_ref() else {
            return;
        };
        for field in [
            "imageInput",
            "pdfInput",
            "temperature",
            "reasoning",
            "contextTokens",
            "outputTokens",
        ] {
            let (suggested, current) = match field {
                "contextTokens" => (
                    suggestion.number(field).map(|value| value.to_string()),
                    capabilities.context_tokens.map(|value| value.to_string()),
                ),
                "outputTokens" => (
                    suggestion.number(field).map(|value| value.to_string()),
                    capabilities.output_tokens.map(|value| value.to_string()),
                ),
                _ => (
                    suggestion.flag(field).map(|value| value.to_string()),
                    match field {
                        "imageInput" => capabilities.image_input,
                        "pdfInput" => capabilities.pdf_input,
                        "temperature" => capabilities.temperature,
                        _ => capabilities.reasoning,
                    }
                    .map(|value| value.to_string()),
                ),
            };
            let Some(suggested) = suggested else {
                continue;
            };
            let adopted = current.as_deref() == Some(suggested.as_str());
            capabilities.set_source(
                field,
                if adopted {
                    vibex_core::ModelCapabilitySource::Catalog
                } else {
                    vibex_core::ModelCapabilitySource::Declared
                },
            );
        }
    }
}

/// Every protocol, offered by the Models tab of a saved record.
///
/// A shared record is not narrowed to one Agent's interfaces, so all of them
/// are listed and none is blocked.
pub(in crate::management) fn all_wire_choices() -> Vec<WireChoice> {
    vibex_core::ProviderModelWireApi::ALL
        .iter()
        .map(|api| WireChoice {
            api: *api,
            support: None,
            supported: true,
        })
        .collect()
}

impl ManagementCenter {
    /// Re-reads the open model into the editor.
    pub(in crate::management) fn load_model_editor(
        &mut self,
        model: &vibex_core::ProviderConfiguredModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model_editor.load(model, window, cx);
        self.model_editor.suggestion = None;
        self.model_editor.suggestion_auto_filled = 0;
    }

    /// Writes the editor into the Agent dialog's draft model, in place.
    ///
    /// The Agent dialog has no Apply step: a field that still parses is the
    /// model's new value, so the whole dialog's Save carries it.
    pub(in crate::management) fn apply_model_editor(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_profile_model_index() else {
            // The Models tab shares this draft but not the Agent's selection;
            // its fields still have to repaint.
            cx.notify();
            return;
        };
        if self.profile_configured_models.get(index).is_none() {
            cx.notify();
            return;
        }
        let Some((id, display_name, mut capabilities)) = self.model_editor.read(cx) else {
            cx.notify();
            return;
        };
        self.model_editor.mark_provenance(&mut capabilities);
        let wire_api = self.model_editor.wire_api;
        let Some(model) = self.profile_configured_models.get_mut(index) else {
            return;
        };
        model.id = id.clone();
        model.display_name = display_name;
        model.wire_api = wire_api;
        model.capabilities = capabilities;
        // The selection names the model by id, so a rename has to carry it or
        // the pane would lose the model it is editing.
        self.profile_model_selection = Some(id);
        self.projection_editor.mark_draft_changed();
        self.navigation.mark_dirty(ManagementSection::Agents, true);
        cx.notify();
    }

    /// Tells the host an edit happened, and writes it through when it owns the
    /// value.
    fn model_editor_changed(&mut self, write_through: bool, cx: &mut Context<Self>) {
        if write_through {
            self.apply_model_editor(cx);
        } else {
            cx.notify();
        }
    }

    /// One Model's settings panel.
    ///
    /// The caller supplies the footer, because what "save" means differs: the
    /// Agent dialog writes the whole dialog at once, the Models tab writes the
    /// record. Everything above it is one body.
    pub(in crate::management) fn render_model_editor(
        &mut self,
        config: ModelEditorConfig<'_>,
        footer: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ModelEditorConfig {
            id_prefix,
            wire_choices,
            editable_id,
            catalog_choices,
            pending,
            shows_enabled,
            write_through,
        } = config;
        // The host decides what a keystroke means, and the field subscriptions
        // installed when the draft was built read it back. Recording it on the
        // draft keeps one editor body shared by both hosts.
        self.model_editor.write_through = write_through;
        let reasoning_disabled = self.model_editor.reasoning_disabled;
        let error_message = self.model_editor.error_message.clone();
        let mut editor = v_flex()
            .debug_selector(|| format!("{id_prefix}-model-editor"))
            .w_full()
            .min_w_0()
            .gap_4()
            .p_3();

        if shows_enabled {
            editor = editor.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .pb_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().font_medium().child(management_locale_text(
                                "Offer this model",
                                "启用该模型",
                                "啟用該模型",
                            )))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(management_locale_text(
                                        "A disabled model keeps its settings but is not offered to the Agent.",
                                        "停用后仍保留配置，但不会提供给 Agent。",
                                        "停用後仍保留設定，但不會提供給 Agent。",
                                    )),
                            ),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("{id_prefix}-model-enabled")))
                            .small()
                            .checked(self.model_editor.enabled)
                            .disabled(pending)
                            .on_click(cx.listener(move |this, checked, _, cx| {
                                this.model_editor.enabled = *checked;
                                this.model_editor_changed(write_through, cx);
                            })),
                    ),
            );
        }

        if !catalog_choices.is_empty() {
            let mut choices = v_flex().w_full().gap_2();
            choices = choices.child(management_choice_group_label(
                management_locale_text(
                    "Model (from the installed Agent)",
                    "模型（来自已安装的 Agent）",
                    "模型（來自已安裝的 Agent）",
                ),
                cx,
            ));
            let mut options = h_flex().w_full().flex_wrap().gap_1p5();
            for (model_id, model_wire_api) in catalog_choices.iter().cloned() {
                let selected = self.model_editor.id.read(cx).value().trim() == model_id;
                let label = model_wire_api.map_or_else(
                    || model_id.clone(),
                    |wire_api| format!("{model_id} · {}", provider_wire_api_label(wire_api)),
                );
                let click_id = model_id.clone();
                options = options.child(management_option_chip(
                    SharedString::from(format!("{id_prefix}-model-catalog-{model_id}")),
                    SharedString::from(label),
                    selected,
                    pending,
                    None,
                    cx.listener(move |this, _, window, cx| {
                        this.model_editor.id.update(cx, |state, cx| {
                            state.set_value(click_id.clone(), window, cx)
                        });
                        if let Some(wire_api) = model_wire_api {
                            this.model_editor.wire_api = Some(wire_api);
                        }
                        this.model_editor_changed(write_through, cx);
                    }),
                    cx,
                ));
            }
            editor = editor.child(choices.child(options));
        } else if editable_id {
            editor = editor.child(management_input_field_with_error(
                management_locale_text("Model ID", "模型 ID", "模型 ID"),
                &self.model_editor.id,
                false,
                error_message.as_deref(),
                cx,
            ));
        }
        editor = editor.child(management_input_field(
            management_locale_text("Display name", "显示名称", "顯示名稱"),
            &self.model_editor.display_name,
            false,
            cx,
        ));

        // The protocol list is the host's: an Agent offers only what it speaks,
        // a shared record offers everything.
        let mut wire_controls = h_flex().w_full().flex_wrap().gap_1p5();
        let choices = std::iter::once(None).chain(wire_choices.into_iter().map(Some));
        for choice in choices {
            let candidate = choice.as_ref().map(|choice| choice.api);
            let selected = self.model_editor.wire_api == candidate;
            let label = choice.as_ref().map_or_else(
                || SharedString::from(management_locale_text("Inherit", "继承", "繼承")),
                |choice| match choice.support.clone() {
                    Some(support) => SharedString::from(format!(
                        "{} · {support}",
                        provider_wire_api_label(choice.api)
                    )),
                    None => SharedString::from(provider_wire_api_label(choice.api)),
                },
            );
            let blocked = choice.as_ref().is_some_and(|choice| !choice.supported);
            wire_controls = wire_controls.child(management_option_chip(
                SharedString::from(format!("{id_prefix}-model-wire-{candidate:?}")),
                label,
                selected,
                pending || blocked,
                blocked.then(|| {
                    SharedString::from(management_locale_text(
                        "This Agent cannot call this interface",
                        "这个 Agent 还无法调用该接口",
                        "這個 Agent 還無法呼叫該介面",
                    ))
                }),
                cx.listener(move |this, _, _, cx| {
                    this.model_editor.wire_api = candidate;
                    this.model_editor_changed(write_through, cx);
                }),
                cx,
            ));
        }
        editor = editor.child(
            v_flex()
                .w_full()
                .gap_1p5()
                .child(management_choice_group_label(
                    management_locale_text("Model API protocol", "模型接口协议", "模型介面協定"),
                    cx,
                ))
                .child(wire_controls),
        );

        // Reasoning is two decisions, so it is two controls: whether the model
        // reasons at all, and which level table its endpoint accepts.
        let mut reasoning = h_flex().w_full().flex_wrap().gap_1p5();
        for (key, label, disabled) in [
            (
                "default",
                management_locale_text(
                    "Follow Agent default",
                    "跟随 Agent 默认",
                    "跟隨 Agent 預設",
                ),
                false,
            ),
            (
                "none",
                management_locale_text("Non-reasoning", "不支持推理", "不支援推理"),
                true,
            ),
        ] {
            let selected = reasoning_disabled == disabled;
            reasoning = reasoning.child(management_option_chip(
                SharedString::from(format!("{id_prefix}-model-reasoning-{key}")),
                SharedString::from(label),
                selected,
                pending,
                None,
                cx.listener(move |this, _, window, cx| {
                    this.model_editor.reasoning_disabled = disabled;
                    this.model_editor
                        .efforts
                        .update(cx, |state, cx| state.set_value("", window, cx));
                    this.model_editor_changed(write_through, cx);
                }),
                cx,
            ));
        }
        editor = editor.child(
            v_flex()
                .w_full()
                .gap_1p5()
                .child(management_choice_group_label(
                    management_locale_text("Reasoning", "推理能力", "推理能力"),
                    cx,
                ))
                .child(reasoning),
        );

        if !reasoning_disabled {
            // The levels are a set the user clicks together, and the field
            // below stays the place a wire spelling is written, so the chips and
            // the field are two views of one declaration.
            let declared = reasoning_level_entries(&self.model_editor.efforts.read(cx).value());
            let mut levels = h_flex().w_full().flex_wrap().gap_1p5();
            for level in vibex_core::ProviderReasoningEffortLevel::ALL {
                let selected = declared.iter().any(|entry| entry.level() == Some(level));
                let name = level.as_str();
                levels = levels.child(management_option_chip(
                    SharedString::from(format!("{id_prefix}-model-efforts-{name}")),
                    SharedString::from(name),
                    selected,
                    pending,
                    Some(SharedString::from(if selected {
                        management_locale_text("Remove this level", "取消该档位", "取消該檔位")
                    } else {
                        management_locale_text("Declare this level", "声明该档位", "宣告該檔位")
                    })),
                    cx.listener(move |this, _, window, cx| {
                        let declaration = toggled_reasoning_levels(
                            &this.model_editor.efforts.read(cx).value(),
                            level,
                        );
                        this.model_editor.reasoning_disabled = false;
                        this.model_editor
                            .efforts
                            .update(cx, |state, cx| state.set_value(declaration, window, cx));
                        this.model_editor_changed(write_through, cx);
                    }),
                    cx,
                ));
            }
            editor = editor.child(
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(management_choice_group_label(
                        management_locale_text("Thinking levels", "思考档位", "思考檔位"),
                        cx,
                    ))
                    .child(levels)
                    .child(management_input_field_with_error(
                        management_locale_text("Declared levels", "档位列表", "檔位清單"),
                        &self.model_editor.efforts,
                        false,
                        error_message.as_deref(),
                        cx,
                    ))
                    .child(management_field_hint(
                        management_locale_text(
                            "Click a level to declare it and click it again to take it back. The field takes any other spelling: levels separated by commas, or level=wire when the endpoint spells one differently. Empty follows the Agent default.",
                            "点击档位即可声明，再次点击取消。也可以在输入框自定义：多个档位用逗号分隔，端点拼写不同时写成 档位=实际值。留空沿用 Agent 默认。",
                            "點擊檔位即可宣告，再次點擊取消。也可以在輸入框自訂：多個檔位用逗號分隔，端點拼寫不同時寫成 檔位=實際值。留空沿用 Agent 預設。",
                        ),
                        cx,
                    )),
            );
        }

        let capability_rows = self.render_model_capabilities(pending, write_through, cx);
        let suggestions = self.render_model_capability_suggestions(write_through, cx);
        editor = editor.child(profile_editor_section(
            management_locale_text("Advanced", "高级", "進階"),
            None,
            v_flex()
                .w_full()
                .gap_2p5()
                .children(capability_rows)
                .child(management_token_field(
                    management_locale_text(
                        "Context window (tokens)",
                        "上下文窗口（Token）",
                        "上下文視窗（Token）",
                    ),
                    &self.model_editor.context_tokens,
                    error_message.as_deref(),
                    &MANAGEMENT_CONTEXT_TOKEN_PRESETS,
                    format!("{id_prefix}-model-context-preset"),
                    pending,
                    cx,
                ))
                .child(management_token_field(
                    management_locale_text(
                        "Max output tokens",
                        "最大输出 Token",
                        "最大輸出 Token",
                    ),
                    &self.model_editor.output_tokens,
                    None,
                    &MANAGEMENT_OUTPUT_TOKEN_PRESETS,
                    format!("{id_prefix}-model-output-preset"),
                    pending,
                    cx,
                ))
                .children(suggestions)
                .child(management_field_hint(
                    management_locale_text(
                        "Click a value to declare it and click it again to clear it, or type any other count. Empty keeps the Agent's own default; only Agents that read a declared limit are affected.",
                        "点击常用值即可填入，再次点击清除，也可以输入其他数值。留空沿用 Agent 默认；只有会读取该设置的 Agent 受影响。",
                        "點擊常用值即可填入，再次點擊清除，也可以輸入其他數值。留空沿用 Agent 預設；只有會讀取該設定的 Agent 受影響。",
                    ),
                    cx,
                ))
                .into_any_element(),
            cx,
        ));
        editor = editor.child(footer);
        editor.into_any_element()
    }

    /// The capability fields a host can read, as three-state choices.
    ///
    /// The catalogue only suggests these; a record with no catalogue entry
    /// still has to be able to declare them.
    fn render_model_capabilities(
        &mut self,
        pending: bool,
        write_through: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let draft = &self.model_editor;
        let fields = [
            (
                "imageInput",
                management_locale_text("Image input", "图片输入", "圖片輸入"),
                draft.image_input,
            ),
            (
                "pdfInput",
                management_locale_text("PDF input", "PDF 输入", "PDF 輸入"),
                draft.pdf_input,
            ),
            (
                "temperature",
                management_locale_text("Honours temperature", "支持温度参数", "支援溫度參數"),
                draft.temperature,
            ),
        ];
        let mut rows = Vec::with_capacity(fields.len());
        for (field, label, value) in fields {
            let mut choices = h_flex().w_full().flex_wrap().gap_1p5();
            for (choice, choice_label) in [
                (
                    None,
                    management_locale_text(
                        "Follow Agent default",
                        "跟随 Agent 默认",
                        "跟隨 Agent 預設",
                    ),
                ),
                (
                    Some(true),
                    management_locale_text("Supported", "支持", "支援"),
                ),
                (
                    Some(false),
                    management_locale_text("Unsupported", "不支持", "不支援"),
                ),
            ] {
                let selected = value == choice;
                choices = choices.child(management_option_chip(
                    SharedString::from(format!(
                        "model-capability-{field}-{}",
                        match choice {
                            None => "inherit",
                            Some(true) => "yes",
                            Some(false) => "no",
                        }
                    )),
                    SharedString::from(choice_label),
                    selected,
                    pending,
                    None,
                    cx.listener(move |this, _, _, cx| {
                        match field {
                            "imageInput" => this.model_editor.image_input = choice,
                            "pdfInput" => this.model_editor.pdf_input = choice,
                            "temperature" => this.model_editor.temperature = choice,
                            _ => return,
                        }
                        this.model_editor_changed(write_through, cx);
                    }),
                    cx,
                ));
            }
            rows.push(
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(management_choice_group_label(label, cx))
                    .child(choices)
                    .into_any_element(),
            );
        }
        rows
    }

    /// The catalogue's values the open model does not hold yet.
    fn render_model_capability_suggestions(
        &mut self,
        write_through: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(suggestion) = self.model_editor.suggestion.clone() else {
            return Vec::new();
        };
        let draft = &self.model_editor;
        let pending = suggestion
            .fields
            .iter()
            .filter(|field| !draft.matches_suggestion(&field.field, &field.value))
            .cloned()
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Vec::new();
        }
        let approximate = suggestion.approximate;
        let auto_filled = self.model_editor.suggestion_auto_filled;
        let mut rows = Vec::new();
        rows.push(
            h_flex()
                .w_full()
                .items_center()
                .justify_end()
                .child(
                    Button::new("model-adopt-all")
                        .xsmall()
                        .outline()
                        .label(management_locale_text(
                            "Apply all suggestions",
                            "采用全部建议",
                            "採用全部建議",
                        ))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.adopt_all_model_capabilities(write_through, window, cx)
                        })),
                )
                .into_any_element(),
        );
        for field in pending {
            let label = match field.field.as_str() {
                "contextTokens" => {
                    management_locale_text("Context window", "上下文窗口", "上下文視窗")
                }
                "outputTokens" => management_locale_text("Max output", "最大输出", "最大輸出"),
                "reasoning" => management_locale_text("Reasoning", "推理", "推理"),
                "imageInput" => management_locale_text("Images", "图片输入", "圖片輸入"),
                "pdfInput" => management_locale_text("PDF", "PDF 输入", "PDF 輸入"),
                "temperature" => management_locale_text("Temperature", "温度参数", "溫度參數"),
                _ => continue,
            };
            let value = field.value.clone();
            let field_name = field.field.clone();
            rows.push(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{label} · {value}")),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "model-adopt-suggestion-{field_name}"
                        )))
                        .xsmall()
                        .ghost()
                        .label(management_locale_text("Apply", "采用", "採用"))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.adopt_model_capability(
                                    &field_name,
                                    &value,
                                    write_through,
                                    window,
                                    cx,
                                )
                            },
                        )),
                    )
                    .into_any_element(),
            );
        }
        if approximate {
            rows.push(management_field_hint(
                management_locale_text(
                    "The catalogue answer is approximate.",
                    "目录答案是近似值。",
                    "目錄答案是近似值。",
                ),
                cx,
            ));
        }
        let note = if auto_filled > 0 {
            management_locale_text(
                "Filled from the catalogue when this model opened. Save to keep it; apply a value to restore it.",
                "打开时已按目录自动填入；保存后生效，点采用可恢复目录值。",
                "開啟時已按目錄自動填入；儲存後生效，點採用可還原目錄值。",
            )
        } else {
            management_locale_text(
                "Click apply to copy a catalogue value into the form. Nothing is written until you save.",
                "点采用即可把目录值写入表单；保存前不会写入记录。",
                "點採用即可把目錄值寫入表單；儲存前不會寫入紀錄。",
            )
        };
        rows.push(management_field_hint(note, cx));
        rows
    }

    fn adopt_model_capability(
        &mut self,
        field: &str,
        value: &str,
        write_through: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = &mut self.model_editor;
        let number = value.parse::<u32>().ok().map(|value| value.to_string());
        let flag = |value: &str| value.parse::<bool>().ok();
        match field {
            "contextTokens" => {
                if let Some(value) = number {
                    draft
                        .context_tokens
                        .update(cx, |state, cx| state.set_value(value, window, cx));
                }
            }
            "outputTokens" => {
                if let Some(value) = number {
                    draft
                        .output_tokens
                        .update(cx, |state, cx| state.set_value(value, window, cx));
                }
            }
            "imageInput" => draft.image_input = flag(value),
            "pdfInput" => draft.pdf_input = flag(value),
            "temperature" => draft.temperature = flag(value),
            "reasoning" => draft.reasoning = flag(value),
            _ => return,
        }
        self.model_editor_changed(write_through, cx);
    }

    fn adopt_all_model_capabilities(
        &mut self,
        write_through: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(suggestion) = self.model_editor.suggestion.clone() else {
            return;
        };
        for field in suggestion.fields {
            self.adopt_model_capability(&field.field, &field.value, write_through, window, cx);
        }
    }
}

/// Whether a declared capability already equals a catalogue answer.
///
/// Kept beside the draft's own check so the test can assert the rule without an
/// app context; the editor still reads the values the user is looking at.
#[cfg(test)]
pub(in crate::management) fn capability_matches_suggestion(
    capabilities: &vibex_core::ProviderModelCapabilities,
    field: &str,
    suggested: &str,
) -> bool {
    let current = match field {
        "contextTokens" => capabilities.context_tokens.map(|value| value.to_string()),
        "outputTokens" => capabilities.output_tokens.map(|value| value.to_string()),
        "reasoning" => capabilities.reasoning.map(|value| value.to_string()),
        "imageInput" => capabilities.image_input.map(|value| value.to_string()),
        "pdfInput" => capabilities.pdf_input.map(|value| value.to_string()),
        "temperature" => capabilities.temperature.map(|value| value.to_string()),
        _ => return false,
    };
    current.as_deref() == Some(suggested)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A suggested value counts as adopted only while the declaration still
    /// equals it; an edited or absent field is the user's own.
    #[test]
    fn a_capability_matches_only_the_same_catalogue_answer() {
        let capabilities = vibex_core::ProviderModelCapabilities {
            context_tokens: Some(128_000),
            reasoning: Some(true),
            temperature: Some(false),
            ..Default::default()
        };
        assert!(capability_matches_suggestion(
            &capabilities,
            "contextTokens",
            "128000"
        ));
        assert!(!capability_matches_suggestion(
            &capabilities,
            "contextTokens",
            "200000"
        ));
        assert!(capability_matches_suggestion(
            &capabilities,
            "reasoning",
            "true"
        ));
        // A false answer is a declaration, not an absent one.
        assert!(capability_matches_suggestion(
            &capabilities,
            "temperature",
            "false"
        ));
        // A field the model does not declare never matches.
        assert!(!capability_matches_suggestion(
            &capabilities,
            "imageInput",
            "true"
        ));
        assert!(!capability_matches_suggestion(
            &capabilities,
            "unknown",
            "1"
        ));
    }
}

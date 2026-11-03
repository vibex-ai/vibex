//! The new-Provider page.
//!
//! Two steps. The first is a gallery: every built-in preset as a tile, with
//! its logo, grouped by kind and searchable, plus a blank "Custom" tile. The
//! second is a short form the preset has already filled: name, address and
//! Key up front, models next, and everything a preset rarely needs folded
//! under Advanced.

use super::*;

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
        let sections = self.provider_picker_preset_sections(&query);
        let available = sections
            .iter()
            .map(|(category, _)| *category)
            .collect::<BTreeSet<_>>();
        let anchors = ProviderPresetCategory::ALL.map(|category| {
            (
                category,
                ScrollAnchor::for_handle(self.workspace_create_scroll.clone()),
            )
        });
        let anchor_for = |category| {
            anchors
                .iter()
                .find_map(|(candidate, anchor)| (*candidate == category).then(|| anchor.clone()))
        };

        let mut chips = h_flex().items_center().gap_1();
        chips = chips.child(
            Button::new("management-provider-create-kind-all")
                .small()
                .ghost()
                .label(management_locale_text("All", "全部", "全部"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.workspace_create_scroll
                        .set_offset(point(px(0.0), px(0.0)));
                    cx.notify();
                })),
        );
        for category in ProviderPresetCategory::ALL {
            let anchor = anchor_for(category).expect("every category owns a scroll anchor");
            let debug_selector = match category {
                ProviderPresetCategory::Vendor => "management-provider-create-kind-vendor",
                ProviderPresetCategory::Relay => "management-provider-create-kind-relay",
                ProviderPresetCategory::Local => "management-provider-create-kind-local",
            };
            chips = chips.child(
                Button::new(SharedString::from(format!(
                    "management-provider-create-kind-{}",
                    category.metadata_value()
                )))
                .debug_selector(move || debug_selector.to_string())
                .small()
                .ghost()
                .label(category.label())
                .disabled(!available.contains(&category))
                .on_click(cx.listener(move |_, _, window, cx| {
                    anchor.scroll_to(window, cx);
                    cx.notify();
                })),
            );
        }

        let custom = h_flex().w_full().min_w_0().child(create_tile(
            SharedString::from("management-provider-create-custom"),
            ws_icon_tile("icons/vibex/wrench.svg", px(42.0), None, cx),
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

        let mut catalog = v_flex().w_full().min_w_0().gap_2();
        for (category, presets) in &sections {
            let mut grid = h_flex().w_full().min_w_0().flex_wrap().gap_3();
            for preset in presets {
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
                    provider_source_logo(preset, px(38.0), cx),
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
            let anchor = anchor_for(*category);
            catalog = catalog.child(
                div()
                    .id(SharedString::from(format!(
                        "management-provider-create-section-{}",
                        category.metadata_value()
                    )))
                    .w_full()
                    .min_w_0()
                    .anchor_scroll(anchor)
                    .child(provider_source_section(
                        SharedString::from(format!(
                            "management-provider-create-{}",
                            category.metadata_value()
                        )),
                        category.icon(),
                        category.label(),
                        grid.into_any_element(),
                        cx,
                    )),
            );
        }

        let footer = ws_form_footer(
            None,
            vec![
                Button::new("management-provider-create-gallery-cancel")
                    .ghost()
                    .label(management_locale_text("Cancel", "取消", "取消"))
                    .on_click(cx.listener(|this, _, _, cx| this.close_provider_create_page(cx)))
                    .into_any_element(),
            ],
            cx,
        );
        ws_authoring_page(
            "management-provider-create-page",
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
                .child(custom)
                .child(catalog)
                .when(sections.is_empty(), |page| {
                    page.child(ws_empty_line(
                        management_locale_text(
                            "No preset matches. Start from Custom instead.",
                            "没有匹配的预设，可以从“自定义”开始。",
                            "沒有符合的預設，可以從「自訂」開始。",
                        ),
                        cx,
                    ))
                })
                .child(footer)
                .into_any_element(),
            cx,
        )
    }

    fn render_workspace_create_form(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
        let preset = self.provider_create_preset.clone();
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

        // The preset row names what filled the form; the fields themselves are
        // the shared body, so only the create-only chrome lives here.
        let start_row = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(match preset.as_ref() {
                        Some(preset) => format!(
                            "{} · {}",
                            management_locale_text(
                                "Start from a preset",
                                "从预设开始",
                                "從預設開始"
                            ),
                            preset.display_name
                        ),
                        None => management_locale_text(
                            "No preset: every field is manual",
                            "未使用预设：全部手动填写",
                            "未使用預設：全部手動填寫",
                        )
                        .to_string(),
                    }),
            )
            .child(
                Button::new("management-provider-create-preset-change")
                    .xsmall()
                    .ghost()
                    .label(management_locale_text(
                        "Change preset",
                        "更换预设",
                        "更換預設",
                    ))
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.workspace_create_choosing = true;
                        cx.notify();
                    })),
            );

        let all_presets = Button::new("management-provider-create-all-presets")
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

        let footer = ws_form_footer_with_leading(
            self.provider_form.error.clone(),
            Some(all_presets),
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
                    .on_click(cx.listener(|this, _, _, cx| this.save_provider_form(cx)))
                    .into_any_element(),
            ],
            cx,
        );

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
        let mut body = self.render_provider_form(cx);
        body = v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(start_row)
            .children(region)
            .child(body)
            .into_any_element();
        ws_authoring_page(
            "management-provider-create-page",
            Some(mark),
            title,
            management_locale_text(
                "A shared record: any Agent can use it once it exists.",
                "共享记录：创建后任何 Agent 都可以使用。",
                "共用記錄：建立後任何 Agent 都可以使用。",
            ),
            ws_stack(vec![body, footer]),
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
    let debug_id = id.to_string();
    h_flex()
        .id(id)
        .debug_selector(move || debug_id.clone())
        // A wrapping row must not stretch the tiles in a short final row.
        .flex_none()
        .w(rems(14.0))
        .max_w_full()
        .items_center()
        .gap_3()
        .p_3()
        .rounded(cx.theme().radius_lg)
        .bg(theme::module_surface(cx.theme().is_dark()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn gallery_category_buttons_scroll_to_their_sections(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (center, cx) = cx.add_window_view(ManagementCenter::new);
        let handle = cx.windows().pop().expect("test window");
        cx.simulate_window_resize(handle, gpui::size(px(1200.0), px(600.0)));

        center.update(cx, |center, _| {
            center.navigation.active = ManagementSection::Providers;
        });
        let _ = cx.update_window(handle, |_, window, cx| {
            center.update(cx, |center, cx| {
                center.open_provider_create_page(None, window, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();

        let local = cx
            .debug_bounds("management-provider-create-kind-local")
            .expect("the Local category button should be laid out");
        assert!(
            local.size.width > px(0.0) && local.size.height > px(0.0),
            "the Local category button needs a real hit target"
        );
        let max_offset = center.read_with(cx, |center, _| {
            center.workspace_create_scroll.max_offset().y
        });
        assert!(
            max_offset > px(0.0),
            "the preset gallery should be taller than its viewport"
        );
        let before = center.read_with(cx, |center, _| center.workspace_create_scroll.offset().y);
        cx.simulate_click(local.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();

        let after = center.read_with(cx, |center, _| center.workspace_create_scroll.offset().y);
        assert!(
            after < before,
            "clicking Local should scroll the gallery down: before={before:?}, after={after:?}"
        );
    }

    #[gpui::test]
    fn gallery_tiles_keep_one_width_across_short_rows(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (center, cx) = cx.add_window_view(ManagementCenter::new);
        let handle = cx.windows().pop().expect("test window");
        cx.simulate_window_resize(handle, gpui::size(px(1200.0), px(600.0)));
        center.update(cx, |center, _| {
            center.navigation.active = ManagementSection::Providers;
        });
        let _ = cx.update_window(handle, |_, window, cx| {
            center.update(cx, |center, cx| {
                center.open_provider_create_page(None, window, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });

        let custom = cx
            .debug_bounds("management-provider-create-custom")
            .expect("the custom tile should be laid out");
        let openai = cx
            .debug_bounds("management-provider-create-preset-openai")
            .expect("a first-row preset tile should be laid out");
        let ollama = cx
            .debug_bounds("management-provider-create-preset-ollama")
            .expect("a short-row preset tile should be laid out");

        assert_eq!(custom.size.width, openai.size.width);
        assert_eq!(ollama.size.width, openai.size.width);
    }
}

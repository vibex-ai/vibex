//! The fetch picker for a saved Provider's catalogue.
//!
//! A fetch used to write the endpoint's whole answer into the record, so every
//! model a vendor listed became a model the Provider offered — including the
//! ones nobody would ever call. The Agent dialog's picker already answers this
//! correctly: discovery fills a list, the reader chooses, and only the choice
//! is written. This module gives the Provider's Models tab the same shape.
//!
//! The draft lives on [`ManagementCenter`] rather than inside the dialog
//! because the dialog builder runs on every frame, and a repaint must not drop
//! a choice the reader already made. Rows are the union of the endpoint's
//! answer and the record's existing Models, so a fetch doubles as an inventory
//! of what is already offered.
//!
//! The settings of one row are edited with the one Model editor every other
//! surface uses. It writes through into this draft while a row is open, which
//! is why [`ManagementCenter::apply_model_editor`] checks for the picker
//! first.

use super::*;

/// One row of the fetch picker.
///
/// The row keeps the catalogue entry so a checked Model carries the endpoint's
/// declaration, and `existing` marks a Model the record already offers —
/// removing one stays the Models tab's own delete command.
#[derive(Debug, Clone)]
pub(in crate::management) struct ProviderModelImportRow {
    pub(in crate::management) model: vibex_core::ModelProviderCatalogEntry,
    pub(in crate::management) chosen: bool,
    pub(in crate::management) existing: bool,
}

/// The catalogue a saved Provider's last fetch offers, and the choices made
/// against it so far.
pub(in crate::management) struct ProviderModelImportDraft {
    pub(in crate::management) profile_id: vibex_core::ModelProviderProfileId,
    pub(in crate::management) profile_name: String,
    pub(in crate::management) rows: Vec<ProviderModelImportRow>,
    /// The row whose settings are open, by Model id.
    pub(in crate::management) expanded: Option<String>,
    /// The last fetch's failure, shown in the body beside its retry command.
    pub(in crate::management) error: Option<String>,
    /// What the last confirm answered without writing, such as an empty
    /// choice. It lives here because a page-level notice sits behind the
    /// dialog and would never be seen.
    pub(in crate::management) hint: Option<String>,
}

impl ProviderModelImportDraft {
    /// How many rows the confirm would actually add.
    pub(in crate::management) fn chosen_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.chosen && !row.existing)
            .count()
    }
}

/// Hosts the fetch picker inside a GPUI dialog.
///
/// The body reads the draft on [`ManagementCenter`] on every frame: a view that
/// rendered a snapshot would lose the reader's checks the moment the dialog
/// layer rebuilt it.
pub(in crate::management) struct ManagementProviderModelPickerDialog {
    center: Entity<ManagementCenter>,
    _center_subscription: Subscription,
}

impl ManagementProviderModelPickerDialog {
    pub(in crate::management) fn new(
        center: Entity<ManagementCenter>,
        cx: &mut Context<Self>,
    ) -> Self {
        let center_subscription = cx.observe(&center, |_, _, cx| cx.notify());
        Self {
            center,
            _center_subscription: center_subscription,
        }
    }
}

impl Render for ManagementProviderModelPickerDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.center.update(cx, |center, cx| {
            center.render_provider_model_import_body(window, cx)
        })
    }
}

/// Whether one picker row matches the picker's own filter.
fn provider_model_import_matches(
    model: &vibex_core::ModelProviderCatalogEntry,
    query: &str,
) -> bool {
    if query.is_empty() {
        return true;
    }
    model.id.to_lowercase().contains(query)
        || model
            .display_name
            .as_deref()
            .is_some_and(|name| name.to_lowercase().contains(query))
}

impl ManagementCenter {
    /// Opens the fetch picker for one saved Provider and asks for its catalogue.
    ///
    /// The dialog opens first and shows the spinner, the way the native import
    /// picker does: the fetch is a network round trip, and a button that only
    /// answered with a banner would leave the reader wondering whether their
    /// click landed.
    pub(in crate::management) fn open_shared_provider_model_picker(
        &mut self,
        profile_id: vibex_core::ModelProviderProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mutation.is_some() {
            return;
        }
        let Some(profile_name) = self
            .providers
            .iter()
            .find(|profile| profile.id == profile_id)
            .map(|profile| profile.display_name.clone())
        else {
            self.error = Some(
                management_locale_text(
                    "The Provider is no longer listed",
                    "该供应商已不在列表中",
                    "該供應商已不在清單中",
                )
                .to_string(),
            );
            cx.notify();
            return;
        };
        // The picker borrows the one Model editor, so the Models tab's inline
        // row closes first: an editor left open behind the dialog would have
        // its draft replaced without the reader seeing it happen.
        if self.provider_model_edit_id.is_some() {
            self.close_shared_provider_model_editor(window, cx);
        }
        self.provider_model_import_search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.model_editor.clear(window, cx);
        self.provider_model_import = Some(ProviderModelImportDraft {
            profile_id: profile_id.clone(),
            profile_name,
            rows: Vec::new(),
            expanded: None,
            error: None,
            hint: None,
        });
        self.present_provider_model_picker(window, cx);
        self.fetch_shared_provider_model_catalogue(profile_id, cx);
    }

    /// Asks the endpoint for its catalogue and fills the open picker with it.
    ///
    /// Nothing is written here. The rows are the answer, and which of them the
    /// Provider offers is the reader's decision, taken by the confirm.
    pub(in crate::management) fn fetch_shared_provider_model_catalogue(
        &mut self,
        profile_id: vibex_core::ModelProviderProfileId,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        if self.mutation.is_some() {
            return;
        }
        self.mutation = Some(ManagementMutation::ProviderProbe(format!(
            "shared-models:{}",
            profile_id.as_str()
        )));
        if let Some(draft) = self.provider_model_import.as_mut() {
            draft.error = None;
        }
        let this = cx.entity();
        let requested = profile_id.clone();
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let outcome = management_off_thread(cx, async move {
                backend
                    .management()
                    .fetch_model_provider_profile_models(
                        vibex_core::ModelProviderFetchModelsRequest {
                            model_provider_profile_id: profile_id,
                        },
                    )
                    .await
            })
            .await;
            this.update(cx, |this, cx| {
                this.mutation = None;
                this.apply_shared_provider_model_catalogue(requested, outcome, cx);
                cx.notify();
            });
        }));
    }

    /// Turns one fetch's answer into picker rows.
    pub(in crate::management) fn apply_shared_provider_model_catalogue(
        &mut self,
        requested: vibex_core::ModelProviderProfileId,
        outcome: vibex_backend::BackendResult<vibex_core::ModelProviderFetchModelsResponse>,
        _cx: &mut Context<Self>,
    ) {
        // A late answer describes a picker that is no longer on screen.
        if self
            .provider_model_import
            .as_ref()
            .is_none_or(|draft| draft.profile_id != requested)
        {
            return;
        }
        match outcome {
            Ok(response) => {
                let Some(profile) = self
                    .providers
                    .iter()
                    .find(|profile| profile.id == response.model_provider_profile_id)
                    .cloned()
                else {
                    return;
                };
                let auto_include = profile.metadata.auto_refresh_models;
                let mut rows = response
                    .models
                    .into_iter()
                    .map(|model| {
                        let existing = profile
                            .configured_models
                            .iter()
                            .any(|configured| configured.id == model.id);
                        ProviderModelImportRow {
                            // AutoInclude is the one setting that says "add what
                            // the vendor lists". It still ends at the confirm, so
                            // its rows arrive checked rather than written.
                            chosen: existing || auto_include,
                            existing,
                            model,
                        }
                    })
                    .collect::<Vec<_>>();
                rows.sort_by(|left, right| {
                    let left_name = left
                        .model
                        .display_name
                        .as_deref()
                        .unwrap_or(&left.model.id)
                        .to_lowercase();
                    let right_name = right
                        .model
                        .display_name
                        .as_deref()
                        .unwrap_or(&right.model.id)
                        .to_lowercase();
                    left_name
                        .cmp(&right_name)
                        .then_with(|| left.model.id.cmp(&right.model.id))
                });
                if let Some(draft) = self.provider_model_import.as_mut() {
                    draft.rows = rows;
                    draft.expanded = None;
                }
            }
            Err(error) => {
                if let Some(draft) = self.provider_model_import.as_mut() {
                    draft.rows.clear();
                    draft.error = Some(format!("{}: {}", error.code, error.message));
                }
            }
        }
    }

    /// Opens the picker's dialog, once per fetch.
    pub(in crate::management) fn present_provider_model_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let center = cx.entity();
        let confirm_center = center.clone();
        let close_center = center.clone();
        let content = cx.new(|cx| ManagementProviderModelPickerDialog::new(center, cx));
        let profile_name = self
            .provider_model_import
            .as_ref()
            .map(|draft| draft.profile_name.clone())
            .unwrap_or_default();
        window.open_dialog(cx, move |dialog, window, cx| {
            // The Agent dialog's picker is the model this one follows, and it is
            // a two-pane surface: a narrow dialog would stack the panes and turn
            // the settings into a second scroll region. It takes the window
            // instead, and reads the size per frame so a resize while it is open
            // is followed rather than left behind.
            let viewport = window.viewport_size();
            let (dialog_width, dialog_height) = (
                (f32::from(viewport.width) - 32.0).max(360.0),
                (f32::from(viewport.height) - 32.0).max(360.0),
            );
            let confirm_center = confirm_center.clone();
            let close_center = close_center.clone();
            let picked = confirm_center
                .read(cx)
                .provider_model_import
                .as_ref()
                .map_or(0, ProviderModelImportDraft::chosen_count);
            let caption = management_locale_text("Fetch models", "拉取模型", "擷取模型");
            let title = SharedString::from(if profile_name.is_empty() {
                caption.to_string()
            } else {
                format!("{caption} · {profile_name}")
            });
            let add_label = management_locale_text("Add models", "添加模型", "新增模型");
            let footer = gpui_component::dialog::DialogFooter::new()
                .child(
                    gpui_component::dialog::DialogClose::new().child(
                        Button::new("management-provider-model-import-cancel")
                            .outline()
                            .label(management_locale_text("Cancel", "取消", "取消")),
                    ),
                )
                .child(
                    gpui_component::dialog::DialogAction::new().child(
                        Button::new("management-provider-model-import-confirm")
                            .primary()
                            .disabled(picked == 0)
                            .label(if picked == 0 {
                                add_label.to_string()
                            } else {
                                format!("{add_label} ({picked})")
                            }),
                    ),
                );
            dialog
                .title(title)
                .w(px(dialog_width))
                .max_w(px(dialog_width))
                .h(px(dialog_height))
                .entrance(DialogEntrance::None)
                .child(content.clone())
                .footer(footer)
                .on_ok(move |_, window, cx| {
                    let mut added = false;
                    confirm_center.update(cx, |this, cx| {
                        added = this.confirm_shared_provider_model_import(window, cx);
                    });
                    added
                })
                .on_close(move |_, _, cx| {
                    close_center.update(cx, |this, cx| this.close_shared_provider_model_import(cx));
                })
        });
    }

    /// Writes the chosen Models and closes the picker.
    ///
    /// Returns whether anything was added, which is what tells the dialog
    /// whether its confirm should close it.
    pub(in crate::management) fn confirm_shared_provider_model_import(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // The open row's last keystroke may still be in flight, so the editor
        // is read once more before the choice is written.
        self.apply_provider_model_import_editor(cx);
        let Some(draft) = self.provider_model_import.as_ref() else {
            return false;
        };
        let chosen = draft
            .rows
            .iter()
            .filter(|row| row.chosen && !row.existing)
            .map(|row| row.model.clone())
            .collect::<Vec<_>>();
        if chosen.is_empty() {
            // The confirm stays open so the choice is still on screen: closing
            // on an empty answer would throw away a catalogue the reader would
            // have to fetch again.
            if let Some(draft) = self.provider_model_import.as_mut() {
                draft.hint = Some(
                    management_locale_text(
                        "Choose at least one model, or cancel to leave the Provider unchanged.",
                        "请至少选择一个模型；也可以取消，供应商保持不变。",
                        "請至少選擇一個模型；也可以取消，供應商保持不變。",
                    )
                    .to_string(),
                );
            }
            cx.notify();
            return false;
        }
        let profile_id = draft.profile_id.clone();
        let Some(mut profile) = self
            .providers
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
        else {
            return false;
        };
        self.provider_model_import = None;
        self.model_editor.clear(window, cx);
        // A chosen Model joins the record; a row the record already offers is
        // left exactly as it is, so its capabilities and enabled state stay the
        // user's. Discovery may add, never overwrite.
        profile.configured_models =
            merge_discovered_provider_models(&profile.configured_models, chosen);
        repair_shared_default_model(&mut profile);
        self.update_shared_provider(
            profile,
            management_locale_text("Models added", "模型已添加", "模型已新增"),
            cx,
        );
        true
    }

    /// Drops the picker's draft when its dialog closes.
    pub(in crate::management) fn close_shared_provider_model_import(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        self.provider_model_import = None;
        self.model_editor.target_id = None;
        cx.notify();
    }

    /// The picker's filter text, lowercased.
    fn provider_model_import_query(&self, cx: &App) -> String {
        self.provider_model_import_search
            .read(cx)
            .value()
            .trim()
            .to_lowercase()
    }

    /// Reads the one Model editor back into the picker's open row.
    ///
    /// Returns whether a row took the values, so [`ManagementCenter::apply_model_editor`]
    /// can tell "the picker owns this edit" from "nothing is open".
    pub(in crate::management) fn apply_provider_model_import_editor(
        &mut self,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(model_id) = self
            .provider_model_import
            .as_ref()
            .and_then(|draft| draft.expanded.clone())
        else {
            return false;
        };
        let Some((_, display_name, mut capabilities)) = self.model_editor.read(cx) else {
            cx.notify();
            return true;
        };
        self.model_editor.mark_provenance(&mut capabilities);
        let wire_api = self.model_editor.wire_api;
        let enabled = self.model_editor.enabled;
        let Some(draft) = self.provider_model_import.as_mut() else {
            return false;
        };
        let Some(row) = draft.rows.iter_mut().find(|row| row.model.id == model_id) else {
            return false;
        };
        row.model.display_name = display_name;
        row.model.enabled = enabled;
        row.model.capabilities = capabilities;
        row.model.metadata.retain(|entry| entry.key != "wireApi");
        if let Some(wire_api) = wire_api {
            row.model
                .metadata
                .push(vibex_core::ProviderBindingMetadata {
                    key: "wireApi".to_string(),
                    value: wire_api.wire_protocol_id().to_string(),
                });
        }
        true
    }

    /// Points the settings pane at one row.
    ///
    /// A click on a row is the same gesture as checking it, exactly as in the
    /// Agent dialog's picker: the Model the reader is looking at is the Model
    /// they mean. A row the record already offers is not editable here — its
    /// settings belong to the Models tab — so clicking it does nothing.
    pub(in crate::management) fn select_provider_model_import_row(
        &mut self,
        model_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(existing) = self
            .provider_model_import
            .as_ref()
            .and_then(|draft| draft.rows.iter().find(|row| row.model.id == model_id))
            .map(|row| row.existing)
        else {
            return;
        };
        if existing {
            return;
        }
        self.open_provider_model_import_row(model_id, window, cx);
    }

    /// Points the settings pane at one row, offering it as it opens.
    pub(in crate::management) fn open_provider_model_import_row(
        &mut self,
        model_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(draft) = self.provider_model_import.as_mut() {
            let Some(row) = draft.rows.iter_mut().find(|row| row.model.id == model_id) else {
                return;
            };
            if row.existing {
                return;
            }
            row.chosen = true;
            draft.expanded = Some(model_id);
        }
        self.load_provider_model_import_editor(window, cx);
        cx.notify();
    }

    /// Checks or unchecks one row.
    ///
    /// Checking a row also opens it: choosing a Model and configuring it are
    /// one task, and the settings are where the choice becomes specific.
    pub(in crate::management) fn toggle_provider_model_import_row(
        &mut self,
        model_id: String,
        chosen: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut opens = false;
        if let Some(draft) = self.provider_model_import.as_mut() {
            let Some(row) = draft.rows.iter_mut().find(|row| row.model.id == model_id) else {
                return;
            };
            if row.existing {
                return;
            }
            row.chosen = chosen;
            draft.hint = None;
            if chosen {
                draft.expanded = Some(model_id);
                opens = true;
            } else if draft.expanded.as_deref() == Some(model_id.as_str()) {
                draft.expanded = None;
            }
        }
        if opens {
            self.load_provider_model_import_editor(window, cx);
        } else {
            self.close_provider_model_import_row(window, cx);
            return;
        }
        cx.notify();
    }

    /// Checks or unchecks every row the filter shows.
    ///
    /// "All" means the rows on screen: a filtered select-all leaves hidden
    /// matches alone, and a row the record already offers is never toggled.
    pub(in crate::management) fn set_visible_provider_model_import_rows(
        &mut self,
        chosen: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = self.provider_model_import_query(cx);
        if let Some(draft) = self.provider_model_import.as_mut() {
            for row in &mut draft.rows {
                if row.existing || !provider_model_import_matches(&row.model, &query) {
                    continue;
                }
                row.chosen = chosen;
            }
            draft.expanded = None;
            draft.hint = None;
        }
        self.model_editor.clear(window, cx);
        cx.notify();
    }

    /// Re-points the settings pane at the draft's open row.
    ///
    /// The Models tab's inline editor is closed here as well as on open: the
    /// two surfaces share one editor, and only one of them may own it.
    fn load_provider_model_import_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.provider_model_import.as_ref().and_then(|draft| {
            let id = draft.expanded.as_deref()?;
            draft
                .rows
                .iter()
                .find(|row| row.model.id == id)
                .map(|row| row.model.clone())
        }) else {
            self.model_editor.clear(window, cx);
            return;
        };
        let configured = provider_configured_model_from_catalog(&model);
        self.load_model_editor(&configured, window, cx);
        self.model_editor.target_id = Some(model.id);
    }

    /// The picker's body: the fetched Models on the left, the chosen Model's
    /// settings on the right.
    ///
    /// It is the Agent dialog's model picker, for a saved Provider: discovery
    /// fills a list, the reader chooses, and the chosen row's settings are
    /// edited beside it. Both panes fill the dialog, so a short catalogue
    /// leaves the reader a real settings pane rather than a blank band.
    pub(in crate::management) fn render_provider_model_import_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(draft) = self.provider_model_import.as_ref() else {
            return div().into_any_element();
        };
        let loading = matches!(
            &self.mutation,
            Some(ManagementMutation::ProviderProbe(action))
                if action == &format!("shared-models:{}", draft.profile_id.as_str())
        );
        let error = draft.error.clone();
        let hint = draft.hint.clone();
        let profile_id = draft.profile_id.clone();
        let wide = f32::from(window.viewport_size().width) >= MANAGEMENT_PROFILE_PANES_MIN_WIDTH;

        let mut body = v_flex().size_full().min_h_0().min_w_0().gap_3();
        if let Some(hint) = hint {
            body = body.child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(hint),
            );
        }
        if let Some(error) = error {
            let retry_id = profile_id.clone();
            return body
                .child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().danger)
                        .p_3()
                        .child(div().text_sm().font_medium().child(management_locale_text(
                            "The model list could not be read",
                            "无法读取模型列表",
                            "無法讀取模型清單",
                        )))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(error),
                        )
                        .child(
                            h_flex().w_full().justify_end().child(
                                Button::new("management-provider-model-import-retry")
                                    .small()
                                    .outline()
                                    .icon(Icon::default().path("icons/vibex/rotate-ccw.svg"))
                                    .label(management_locale_text("Retry", "重试", "重試"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.fetch_shared_provider_model_catalogue(
                                            retry_id.clone(),
                                            cx,
                                        )
                                    })),
                            ),
                        ),
                )
                .into_any_element();
        }

        if loading {
            return body
                .child(
                    h_flex()
                        .w_full()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .py_10()
                        .child(Spinner::new().with_size(Size::Small))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(management_locale_text(
                                    "Reading the Provider's model list…",
                                    "正在读取供应商的模型列表…",
                                    "正在讀取供應商的模型清單…",
                                )),
                        ),
                )
                .into_any_element();
        }

        // The two panes share the dialog's remaining height, so the list keeps
        // a bounded well under its rows and the settings get the rest.
        let panes = if wide {
            h_flex()
                .w_full()
                .flex_1()
                .min_h_0()
                .items_stretch()
                .gap_3()
                .child(self.render_provider_model_import_candidates_pane(true, cx))
                .child(self.render_provider_model_import_settings_pane(true, window, cx))
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .flex_1()
                .min_h_0()
                .gap_3()
                .child(self.render_provider_model_import_candidates_pane(false, cx))
                .child(self.render_provider_model_import_settings_pane(false, window, cx))
                .into_any_element()
        };
        body.child(panes).into_any_element()
    }

    /// The left pane: what the fetch answered, with the checkbox that decides
    /// which of those Models the Provider should offer.
    fn render_provider_model_import_candidates_pane(
        &mut self,
        bounded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(draft) = self.provider_model_import.as_ref() else {
            return div().into_any_element();
        };
        let profile_id = draft.profile_id.clone();
        let total = draft.rows.len();
        let existing = draft.rows.iter().filter(|row| row.existing).count();
        let chosen = draft.chosen_count();
        let rows = draft.rows.clone();
        let expanded = draft.expanded.clone();
        let query = self.provider_model_import_query(cx);
        let visible = rows
            .iter()
            .filter(|row| provider_model_import_matches(&row.model, &query))
            .cloned()
            .collect::<Vec<_>>();
        let selectable = visible.iter().filter(|row| !row.existing).count();
        let all_visible_chosen = selectable > 0
            && visible
                .iter()
                .filter(|row| !row.existing)
                .all(|row| row.chosen);

        let fetching = matches!(
            &self.mutation,
            Some(ManagementMutation::ProviderProbe(action))
                if action == &format!("shared-models:{}", profile_id.as_str())
        );
        let offered = if chosen == 0 {
            management_locale_text("None chosen", "尚未选择", "尚未選擇").to_string()
        } else {
            management_model_count(chosen)
        };
        let fetch = Button::new("management-provider-model-import-refetch")
            .xsmall()
            .outline()
            .icon(Icon::default().path("icons/vibex/rotate-ccw.svg"))
            .label(management_fetch_models_label())
            .loading(fetching)
            .disabled(self.mutation.is_some())
            .on_click(cx.listener({
                let id = profile_id.clone();
                move |this, _, _, cx| this.fetch_shared_provider_model_catalogue(id.clone(), cx)
            }));
        let header = profile_pane_header(
            management_locale_text("Available models", "可选模型", "可選模型"),
            Some(offered.into()),
            Some(fetch.into_any_element()),
            cx,
        );

        let mut controls = h_flex()
            .w_full()
            .min_w_0()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(management_search_input_compact(
                        &self.provider_model_import_search,
                        cx,
                    )),
            )
            .child(
                Checkbox::new("management-provider-model-import-select-all")
                    .small()
                    .checked(all_visible_chosen)
                    .accessibility_label(management_locale_text("Select all", "全选", "全選"))
                    .disabled(selectable == 0)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(management_locale_text("Select all", "全选", "全選")),
                    )
                    .on_click(cx.listener(move |this, checked, window, cx| {
                        this.set_visible_provider_model_import_rows(*checked, window, cx);
                    })),
            );
        if existing > 0 {
            controls = controls.child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} · {}",
                        management_model_count(total),
                        management_locale_text("already offered", "已提供", "已提供")
                    )),
            );
        }

        let mut list = v_flex().w_full().min_w_0().gap_0p5().p_1();
        if visible.is_empty() {
            list = list.child(bare_empty_state(
                management_locale_text(
                    if total == 0 {
                        "The endpoint listed no models"
                    } else {
                        "No model matches this filter"
                    },
                    if total == 0 {
                        "接口没有返回模型"
                    } else {
                        "没有匹配该筛选的模型"
                    },
                    if total == 0 {
                        "介面沒有回傳模型"
                    } else {
                        "沒有符合這個篩選的模型"
                    },
                ),
                management_locale_text(
                    if total == 0 {
                        "Add a model id from the Models tab instead."
                    } else {
                        "Clear the filter to see the fetched models again."
                    },
                    if total == 0 {
                        "可以在“模型”标签页直接添加模型 ID。"
                    } else {
                        "清除筛选后可重新看到拉取到的模型。"
                    },
                    if total == 0 {
                        "可以在「模型」標籤頁直接新增模型 ID。"
                    } else {
                        "清除篩選後可重新看到擷取到的模型。"
                    },
                ),
                cx,
            ));
        }
        for row in &visible {
            let model_id = row.model.id.clone();
            let row_chosen = row.chosen;
            let is_existing = row.existing;
            let opened = expanded.as_deref() == Some(model_id.as_str());
            let toggled_id = model_id.clone();
            let clicked_id = model_id.clone();
            let title = row
                .model
                .display_name
                .clone()
                .unwrap_or_else(|| row.model.id.clone());
            let mut meta = Vec::new();
            if row.model.display_name.is_some() {
                meta.push(row.model.id.clone());
            }
            if let Some(wire_api) = shared_model_wire_api(&row.model) {
                meta.push(provider_wire_api_label(wire_api).to_string());
            }
            let limits = management_model_limits_label(&row.model.capabilities);
            if !limits.is_empty() {
                meta.push(limits);
            }
            let meta = meta.join(" · ");
            let debug_id = model_id.clone();
            list =
                list.child(
                    h_flex()
                        .id(SharedString::from(format!(
                            "management-provider-model-import-row-{model_id}"
                        )))
                        .debug_selector(move || {
                            format!("management-provider-model-import-row-{debug_id}")
                        })
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .rounded(px(4.0))
                        // The leading edge is reserved at rest so the row does not
                        // shift when it becomes the one the settings pane shows.
                        .border_l_2()
                        .pl_1p5()
                        .pr_2()
                        .py_1p5()
                        .border_color(if opened {
                            cx.theme().primary.opacity(0.75)
                        } else {
                            cx.theme().transparent
                        })
                        .when(opened, |row| {
                            row.bg(cx
                                .theme()
                                .primary
                                .opacity(MANAGEMENT_PANEL_ROW_SELECTED_WASH))
                        })
                        .when(!opened, |row| {
                            row.hover(|row| {
                                row.bg(management_surface_wash(cx, MANAGEMENT_PANEL_ROW_HOVER_WASH))
                            })
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_provider_model_import_row(clicked_id.clone(), window, cx)
                        }))
                        .child(
                            // The checkbox answers the same question the row does,
                            // and it is the one control that can release a choice.
                            div()
                                .id(SharedString::from(format!(
                                    "management-provider-model-import-check-wrap-{model_id}"
                                )))
                                .flex_none()
                                .on_click(|_, _, cx| cx.stop_propagation())
                                .child(
                                    Checkbox::new(SharedString::from(format!(
                                        "management-provider-model-import-check-{model_id}"
                                    )))
                                    .small()
                                    .checked(row_chosen)
                                    .disabled(is_existing)
                                    .accessibility_label(SharedString::from(title.clone()))
                                    .on_click(cx.listener(move |this, checked, window, cx| {
                                        this.toggle_provider_model_import_row(
                                            toggled_id.clone(),
                                            *checked,
                                            window,
                                            cx,
                                        )
                                    })),
                                ),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap_0p5()
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .font_medium()
                                        .when(!row_chosen, |text| {
                                            text.text_color(cx.theme().foreground.opacity(0.75))
                                        })
                                        .child(title),
                                )
                                .when(!meta.is_empty(), |column| {
                                    column.child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(meta),
                                    )
                                }),
                        )
                        .when(is_existing, |row| {
                            row.child(div().flex_none().child(
                                Tag::color(ColorName::Neutral).xsmall().child(
                                    management_locale_text("Already offered", "已提供", "已提供"),
                                ),
                            ))
                        }),
                );
        }

        let list = if bounded {
            div()
                .id("management-provider-model-import-list")
                .debug_selector(|| "management-provider-model-import-list".to_string())
                .w_full()
                .min_w_0()
                .min_h_0()
                .flex_1()
                .overflow_y_scrollbar()
                .scroll_gutter()
                .child(list)
                .into_any_element()
        } else {
            div()
                .id("management-provider-model-import-list")
                .debug_selector(|| "management-provider-model-import-list".to_string())
                .w_full()
                .min_w_0()
                .max_h(px(MANAGEMENT_PROFILE_PANE_MAX_HEIGHT))
                .overflow_y_scroll()
                .scroll_gutter()
                .child(list)
                .into_any_element()
        };

        let body = v_flex()
            .w_full()
            .min_h_0()
            .min_w_0()
            .flex_1()
            .child(controls)
            .child(list)
            .into_any_element();
        profile_pane_shell(
            "provider-model-import-candidates-pane",
            bounded,
            header,
            body,
            None,
            cx,
        )
    }

    /// The right pane: the settings of the Model the list is showing.
    ///
    /// It is the one Model editor, so a Model chosen here is configured with
    /// the same fields and rules as one edited on the Models tab. The edit
    /// writes through into the picker's row rather than the saved record.
    fn render_provider_model_import_settings_pane(
        &mut self,
        bounded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selection = self
            .provider_model_import
            .as_ref()
            .and_then(|draft| draft.expanded.clone());
        let selected_title = selection.as_deref().and_then(|id| {
            self.provider_model_import
                .as_ref()?
                .rows
                .iter()
                .find(|row| row.model.id == id)
                .map(|row| {
                    row.model
                        .display_name
                        .clone()
                        .unwrap_or_else(|| row.model.id.clone())
                })
        });
        let header = profile_pane_header(
            management_locale_text("Model settings", "模型设置", "模型設定"),
            selected_title.clone().map(SharedString::from),
            None,
            cx,
        );

        let body = match selected_title {
            Some(_) => {
                // The editor is pointed at the selected row on every frame, so
                // a row the reader just clicked never shows the previous
                // Model's values.
                if self.model_editor.target_id.as_deref() != selection.as_deref() {
                    self.load_provider_model_import_editor(window, cx);
                }
                let chosen = self
                    .provider_model_import
                    .as_ref()
                    .and_then(|draft| {
                        let id = draft.expanded.as_deref()?;
                        draft
                            .rows
                            .iter()
                            .find(|row| row.model.id == id)
                            .map(|row| row.chosen)
                    })
                    .unwrap_or(false);
                let selection_id = selection.clone().unwrap_or_default();
                let footer = h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if chosen {
                                management_locale_text(
                                    "This model will be added when you confirm.",
                                    "确认后会添加该模型。",
                                    "確認後會新增該模型。",
                                )
                            } else {
                                management_locale_text(
                                    "This model will not be added.",
                                    "该模型不会被添加。",
                                    "該模型不會被新增。",
                                )
                            }),
                    )
                    .child(
                        Button::new("management-provider-model-import-add-selected")
                            .xsmall()
                            .secondary()
                            .icon(IconName::Plus)
                            .label(management_locale_text(
                                "Add this model",
                                "添加该模型",
                                "新增該模型",
                            ))
                            .disabled(chosen)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.toggle_provider_model_import_row(
                                    selection_id.clone(),
                                    true,
                                    window,
                                    cx,
                                )
                            })),
                    );
                let editor = self.render_model_editor(
                    provider_workspace::ModelEditorConfig {
                        id_prefix: "provider-model-import",
                        wire_choices: provider_workspace::all_wire_choices(),
                        editable_id: false,
                        catalog_choices: Vec::new(),
                        pending: false,
                        // The same switch the Agent dialog's picker offers: a
                        // chosen Model can join the record already turned off.
                        shows_enabled: true,
                        write_through: true,
                    },
                    footer.into_any_element(),
                    cx,
                );
                if bounded {
                    v_flex()
                        .w_full()
                        .min_h_0()
                        .flex_1()
                        .overflow_y_scrollbar()
                        .scroll_gutter()
                        .child(editor)
                        .into_any_element()
                } else {
                    editor
                }
            }
            None => bare_empty_state(
                management_locale_text("No model selected", "未选择模型", "未選擇模型"),
                management_locale_text(
                    "Pick a model on the left to set it up before adding it.",
                    "在左侧点选模型，这里可以调整它的设置再添加。",
                    "在左側點選模型，這裡可以調整它的設定再新增。",
                ),
                cx,
            ),
        };

        profile_pane_shell(
            "provider-model-import-settings-pane",
            bounded,
            header,
            body,
            None,
            cx,
        )
    }

    /// Closes the picker's open row, keeping what was typed into it.
    pub(in crate::management) fn close_provider_model_import_row(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(draft) = self.provider_model_import.as_mut() {
            draft.expanded = None;
        }
        self.model_editor.clear(window, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    const SOURCE: &str = include_str!("model_import.rs");
    const PROVIDER: &str = include_str!("provider.rs");

    /// A fetch must never write the whole vendor list: the picker is the step
    /// that lets the reader choose, exactly as the Agent dialog's does.
    #[test]
    fn the_models_tab_fetches_into_a_picker_instead_of_writing_every_model() {
        assert!(
            SOURCE.contains("fn open_shared_provider_model_picker("),
            "the Models tab needs a picker entry point"
        );
        assert!(
            SOURCE.contains("ProviderModelImportRow"),
            "the picker keeps one row per fetched Model"
        );
        // The rows start unchecked unless AutoInclude asks otherwise, so a
        // fetch on its own cannot add a Model the reader did not pick.
        assert!(
            SOURCE.contains("chosen: existing || auto_include"),
            "only the auto-include setting may pre-check a fetched Model"
        );
        // The write happens at the confirm, and it adds rather than replaces.
        assert!(
            SOURCE.contains("merge_discovered_provider_models(&profile.configured_models, chosen)"),
            "confirming the picker must add the chosen Models to the record"
        );
        // The Models tab's own button routes into the picker.
        assert!(
            PROVIDER.contains("open_shared_provider_model_picker(id.clone(), window, cx)"),
            "the Models tab's fetch button opens the picker"
        );
    }

    /// The picker offers the same per-Model settings the Agent dialog does, so
    /// a chosen Model is configured before it is written.
    #[test]
    fn the_picker_edits_each_model_before_it_is_added() {
        for marker in [
            "render_provider_model_import_settings(",
            "ModelEditorConfig {",
            "id_prefix: \"provider-model-import\"",
            "write_through: true",
            "open_provider_model_import_row(",
            "toggle_provider_model_import_row(",
            "set_visible_provider_model_import_rows(",
        ] {
            assert!(SOURCE.contains(marker), "the picker must keep {marker}");
        }
        // The editor writes into the picker's row, not the saved record, so an
        // abandoned picker leaves the Provider untouched.
        assert!(
            SOURCE.contains("fn apply_provider_model_import_editor("),
            "the picker owns its own editor target"
        );
    }

    /// A row the record already offers is shown but cannot be toggled here:
    /// removing a Model stays the Models tab's delete command.
    #[test]
    fn the_picker_marks_models_the_provider_already_offers() {
        assert!(SOURCE.contains("existing: bool"));
        assert!(SOURCE.contains("Already offered"));
        assert!(
            SOURCE.contains("if row.existing {"),
            "an existing row must not be toggled by the picker"
        );
    }

    /// The picker is a dialog, so its check state has to outlive a repaint:
    /// the builder runs on every frame.
    #[test]
    fn the_picker_keeps_its_choices_outside_the_dialog_builder() {
        assert!(
            SOURCE.contains("self.provider_model_import.as_ref()"),
            "the body reads the draft on the center, not a captured snapshot"
        );
        assert!(
            SOURCE.contains("struct ManagementProviderModelPickerDialog"),
            "the dialog body is its own view"
        );
    }
}

#[cfg(test)]
mod picker_tests {
    use super::*;

    fn entry(id: &str, name: Option<&str>) -> vibex_core::ModelProviderCatalogEntry {
        vibex_core::ModelProviderCatalogEntry {
            id: id.to_string(),
            display_name: name.map(str::to_string),
            enabled: true,
            metadata: Vec::new(),
            capabilities: Default::default(),
        }
    }

    fn profile(
        models: Vec<vibex_core::ModelProviderCatalogEntry>,
    ) -> vibex_core::ModelProviderProfile {
        vibex_core::ModelProviderProfile {
            display_name: "Vendor".to_string(),
            configured_models: models,
            status: vibex_core::ModelProviderProfileStatus::Enabled,
            ..Default::default()
        }
    }

    /// A fetch answer becomes rows, and only AutoInclude may pre-check one.
    #[gpui::test]
    fn a_fetch_answer_fills_rows_and_only_auto_include_pre_checks_them(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (center, cx) = cx.add_window_view(ManagementCenter::new);
        let existing = entry("already-offered", Some("Already offered"));
        let profile = profile(vec![existing.clone()]);
        let profile_id = profile.id.clone();
        cx.update(|_, cx| {
            center.update(cx, |center, cx| {
                center.providers = vec![profile];
                center.provider_model_import = Some(ProviderModelImportDraft {
                    profile_id: profile_id.clone(),
                    profile_name: "Vendor".to_string(),
                    rows: Vec::new(),
                    expanded: None,
                    error: None,
                    hint: None,
                });
                center.apply_shared_provider_model_catalogue(
                    profile_id.clone(),
                    Ok(vibex_core::ModelProviderFetchModelsResponse {
                        model_provider_profile_id: profile_id.clone(),
                        models: vec![
                            existing,
                            entry("new-a", Some("A")),
                            entry("new-b", Some("B")),
                        ],
                        diagnostics: Vec::new(),
                    }),
                    cx,
                );
            });
        });
        let draft = center.read_with(cx, |center, _| {
            center
                .provider_model_import
                .as_ref()
                .map(|draft| {
                    draft
                        .rows
                        .iter()
                        .map(|row| (row.model.id.clone(), row.chosen, row.existing))
                        .collect::<Vec<_>>()
                })
                .expect("the picker keeps its draft")
        });
        // The already-offered row is shown, marked, and checked because it is
        // already part of the record. Rows are sorted by name, so the assertion
        // looks the row up rather than assuming a position.
        let already = draft
            .iter()
            .find(|(id, _, _)| id == "already-offered")
            .expect("the already-offered row is listed");
        assert_eq!(
            already,
            &("already-offered".to_string(), true, true),
            "a Model the Provider already offers is marked, not toggled"
        );
        // The vendor's new Models arrive unchecked: without AutoInclude a
        // fetch must not add anything on its own.
        for (id, chosen, existing) in draft.iter().filter(|(id, _, _)| id != "already-offered") {
            assert!(!chosen, "{id} must not arrive checked without AutoInclude");
            assert!(!existing, "{id} is new to the record");
        }
    }

    /// The picker renders its rows and adds a choice when one is checked.
    #[gpui::test]
    fn checking_a_picker_row_offers_it(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (center, cx) = cx.add_window_view(ManagementCenter::new);
        let profile = profile(Vec::new());
        let profile_id = profile.id.clone();
        cx.update(|_, cx| {
            center.update(cx, |center, cx| {
                center.navigation.active = ManagementSection::Providers;
                center.selected_global_provider_id = Some(profile_id.as_str().to_string());
                center.provider_detail_tab = ProviderDetailTab::Models;
                center.providers = vec![profile];
                center.provider_model_import = Some(ProviderModelImportDraft {
                    profile_id: profile_id.clone(),
                    profile_name: "Vendor".to_string(),
                    rows: vec![
                        ProviderModelImportRow {
                            model: entry("alpha", Some("Alpha")),
                            chosen: false,
                            existing: false,
                        },
                        ProviderModelImportRow {
                            model: entry("beta", Some("Beta")),
                            chosen: false,
                            existing: false,
                        },
                    ],
                    expanded: None,
                    error: None,
                    hint: None,
                });
                cx.notify();
            });
        });
        cx.simulate_resize(gpui::size(px(1280.0), px(900.0)));
        for _ in 0..3 {
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            cx.run_until_parked();
        }
        cx.update(|window, cx| {
            center.update(cx, |center, cx| {
                center.toggle_provider_model_import_row("alpha".to_string(), true, window, cx);
            });
        });
        let state = center.read_with(cx, |center, _| {
            let draft = center
                .provider_model_import
                .as_ref()
                .expect("the picker stays open");
            (
                draft.chosen_count(),
                draft.expanded.clone(),
                draft
                    .rows
                    .iter()
                    .map(|row| (row.model.id.clone(), row.chosen))
                    .collect::<Vec<_>>(),
            )
        });
        assert_eq!(state.0, 1, "one chosen row is one Model the confirm adds");
        assert_eq!(state.1.as_deref(), Some("alpha"), "the chosen row opens");
        assert!(
            state.2.iter().any(|(id, chosen)| id == "alpha" && *chosen),
            "checking a row chooses it"
        );
        assert!(
            state.2.iter().any(|(id, chosen)| id == "beta" && !chosen),
            "checking one row leaves its neighbour alone"
        );
    }

    /// The picker is the Agent dialog's two-pane surface, and it fills the
    /// dialog: a short catalogue must not leave a blank band under the list.
    #[gpui::test]
    fn the_picker_fills_the_dialog_with_a_list_and_a_settings_pane(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        // A dialog needs the kit's root, so the test builds the same window the
        // app does rather than a bare view.
        let mut built = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let center = cx.new(|cx| ManagementCenter::new(window, cx));
            let root = gpui_base::Root::new(center.clone(), window, cx);
            built = Some(center);
            root
        });
        let center = built.expect("the management window builds its center");
        let profile = profile(Vec::new());
        let profile_id = profile.id.clone();
        cx.update(|window, cx| {
            center.update(cx, |center, cx| {
                center.navigation.active = ManagementSection::Providers;
                center.selected_global_provider_id = Some(profile_id.as_str().to_string());
                center.provider_detail_tab = ProviderDetailTab::Models;
                center.providers = vec![profile];
                center.provider_model_import = Some(ProviderModelImportDraft {
                    profile_id: profile_id.clone(),
                    profile_name: "Vendor".to_string(),
                    rows: vec![ProviderModelImportRow {
                        model: entry("alpha", Some("Alpha")),
                        chosen: false,
                        existing: false,
                    }],
                    expanded: Some("alpha".to_string()),
                    error: None,
                    hint: None,
                });
                center.present_provider_model_picker(window, cx);
                cx.notify();
            });
        });
        cx.simulate_resize(gpui::size(px(1440.0), px(900.0)));
        for _ in 0..3 {
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            cx.run_until_parked();
        }
        let list = cx
            .debug_bounds("provider-model-import-candidates-pane")
            .expect("the picker draws its candidates pane");
        let settings = cx
            .debug_bounds("provider-model-import-settings-pane")
            .expect("the picker draws its settings pane");
        let editor = cx
            .debug_bounds("provider-model-import-model-editor")
            .expect("the chosen row's settings are drawn");
        // Side by side, and the list on the leading edge.
        assert!(
            list.right() <= settings.left() + px(1.0),
            "the two panes sit beside each other: list={list:?} settings={settings:?}"
        );
        // Both panes take the dialog's height rather than their content's.
        assert!(
            (list.size.height - settings.size.height).abs() <= px(1.0),
            "the panes share the dialog's height: list={list:?} settings={settings:?}"
        );
        assert!(
            list.size.height > px(420.0),
            "a one-row catalogue still fills the pane: {list:?}"
        );
        assert!(
            editor.top() >= settings.top() && editor.bottom() <= settings.bottom() + px(1.0),
            "the settings stay inside their pane: editor={editor:?} pane={settings:?}"
        );
    }

    /// The picker's own filter narrows the rows without touching the choice.
    #[gpui::test]
    fn the_picker_filter_narrows_rows_and_select_all_respects_it(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (center, cx) = cx.add_window_view(ManagementCenter::new);
        let profile = profile(Vec::new());
        let profile_id = profile.id.clone();
        cx.update(|window, cx| {
            center.update(cx, |center, cx| {
                center.providers = vec![profile];
                center.provider_model_import = Some(ProviderModelImportDraft {
                    profile_id: profile_id.clone(),
                    profile_name: "Vendor".to_string(),
                    rows: vec![
                        ProviderModelImportRow {
                            model: entry("alpha", Some("Alpha")),
                            chosen: false,
                            existing: false,
                        },
                        ProviderModelImportRow {
                            model: entry("beta", Some("Beta")),
                            chosen: false,
                            existing: false,
                        },
                    ],
                    expanded: None,
                    error: None,
                    hint: None,
                });
                center
                    .provider_model_import_search
                    .update(cx, |state, cx| state.set_value("alpha", window, cx));
                cx.notify();
            });
        });
        let filtered = center.read_with(cx, |center, cx| {
            let query = center.provider_model_import_query(cx);
            center
                .provider_model_import
                .as_ref()
                .expect("the picker keeps its draft")
                .rows
                .iter()
                .filter(|row| provider_model_import_matches(&row.model, &query))
                .map(|row| row.model.id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(filtered, vec!["alpha".to_string()]);

        // Select-all means the rows the filter shows, so `beta` keeps the state
        // the reader gave it rather than being swept up by a hidden row.
        cx.update(|window, cx| {
            center.update(cx, |center, cx| {
                center.set_visible_provider_model_import_rows(true, window, cx);
            });
        });
        let rows = center.read_with(cx, |center, _| {
            center
                .provider_model_import
                .as_ref()
                .expect("the picker keeps its draft")
                .rows
                .iter()
                .map(|row| (row.model.id.clone(), row.chosen))
                .collect::<Vec<_>>()
        });
        assert!(
            rows.iter().any(|(id, chosen)| id == "alpha" && *chosen),
            "select all checks the visible row"
        );
        assert!(
            rows.iter().any(|(id, chosen)| id == "beta" && !chosen),
            "select all must leave the filtered-out row alone"
        );
    }
}

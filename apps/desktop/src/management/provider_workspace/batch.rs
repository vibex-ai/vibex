//! Multi-select and batch commands for the Provider column.
//!
//! Providers and route groups are edited one at a time on their own pages, but
//! the two lists are the place a reader notices "these five are all disabled" or
//! "this import created a dozen duplicates". Selecting is therefore a mode of
//! the left column, not a second page: while it is on, a card click toggles the
//! record instead of navigating, and one strip above the list applies the same
//! enable, disable, or delete to everything selected.

use super::*;

/// What one batch command does to every selected record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::management) enum WorkspaceBatchAction {
    Enable,
    Disable,
    Delete,
}

impl WorkspaceBatchAction {
    fn label(self) -> &'static str {
        match self {
            Self::Enable => management_locale_text("Enable", "启用", "啟用"),
            Self::Disable => management_locale_text("Disable", "停用", "停用"),
            Self::Delete => management_locale_text("Delete", "删除", "刪除"),
        }
    }
}

impl ManagementCenter {
    /// Whether the column is currently selecting records.
    pub(super) fn workspace_selecting(&self) -> bool {
        self.workspace_selecting
    }

    /// Drops selections that point at records which no longer exist.
    ///
    /// A batch runs against the live lists, so a stale id would inflate the
    /// count while acting on nothing. Reconciling on render keeps the strip's
    /// number true after a delete, an import, or a refresh from another window.
    pub(super) fn reconcile_workspace_selection(&mut self) {
        let provider_ids = self
            .providers
            .iter()
            .map(|profile| profile.id.as_str().to_string())
            .collect::<BTreeSet<_>>();
        let route_ids = self
            .model_routes
            .iter()
            .filter(|route| route.deleted_at_ms.is_none())
            .map(|route| route.id.as_str().to_string())
            .collect::<BTreeSet<_>>();
        self.workspace_selected_providers
            .retain(|id| provider_ids.contains(id));
        self.workspace_selected_routes
            .retain(|id| route_ids.contains(id));
    }

    /// How many records the current selection holds.
    pub(super) fn workspace_selection_count(&self) -> usize {
        self.workspace_selected_providers.len() + self.workspace_selected_routes.len()
    }

    pub(super) fn workspace_provider_selected(&self, id: &str) -> bool {
        self.workspace_selected_providers.contains(id)
    }

    pub(super) fn workspace_route_selected(&self, id: &str) -> bool {
        self.workspace_selected_routes.contains(id)
    }

    /// Turns the selection mode on or off.
    ///
    /// Leaving the mode drops the selection: it is a working set for a batch,
    /// not a bookmark, and a hidden selection that resurfaced later would act
    /// on records the reader is no longer looking at.
    pub(super) fn toggle_workspace_selecting(&mut self, cx: &mut Context<Self>) {
        self.workspace_selecting = !self.workspace_selecting;
        if !self.workspace_selecting {
            self.workspace_selected_providers.clear();
            self.workspace_selected_routes.clear();
        }
        cx.notify();
    }

    pub(super) fn toggle_workspace_provider_selected(
        &mut self,
        id: String,
        cx: &mut Context<Self>,
    ) {
        if !self.workspace_selected_providers.remove(&id) {
            self.workspace_selected_providers.insert(id);
        }
        cx.notify();
    }

    pub(super) fn toggle_workspace_route_selected(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.workspace_selected_routes.remove(&id) {
            self.workspace_selected_routes.insert(id);
        }
        cx.notify();
    }

    /// Selects every record the column currently lists, or clears it when all
    /// of them already are.
    fn toggle_workspace_select_all(
        &mut self,
        provider_ids: &[String],
        route_ids: &[String],
        cx: &mut Context<Self>,
    ) {
        let all_selected = provider_ids
            .iter()
            .all(|id| self.workspace_selected_providers.contains(id))
            && route_ids
                .iter()
                .all(|id| self.workspace_selected_routes.contains(id))
            && self.workspace_selection_count() > 0;
        self.workspace_selected_providers.clear();
        self.workspace_selected_routes.clear();
        if !all_selected {
            self.workspace_selected_providers
                .extend(provider_ids.iter().cloned());
            self.workspace_selected_routes
                .extend(route_ids.iter().cloned());
        }
        cx.notify();
    }

    /// The strip above the list that acts on the selection.
    pub(super) fn render_workspace_batch_bar(
        &mut self,
        provider_ids: &[String],
        route_ids: &[String],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = self.workspace_selection_count();
        let has_records = !provider_ids.is_empty() || !route_ids.is_empty();
        let pending = self.mutation.is_some();
        h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .items_center()
            .gap_1()
            .rounded(px(8.0))
            .border_1()
            .border_color(cx.theme().border.opacity(0.65))
            .bg(cx.theme().muted.opacity(0.25))
            .px_2()
            .py_1p5()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(match count {
                        0 => management_locale_text(
                            "Select records to act on",
                            "选择要批量操作的记录",
                            "選擇要批次操作的記錄",
                        )
                        .to_string(),
                        count => match locale::current_locale() {
                            ResolvedLocale::En => format!("{count} selected"),
                            ResolvedLocale::ZhCn => format!("已选择 {count} 条"),
                            ResolvedLocale::ZhTw => format!("已選取 {count} 筆"),
                        },
                    }),
            )
            .when(has_records, |bar| {
                bar.child(
                    Button::new("management-workspace-select-all")
                        .xsmall()
                        .ghost()
                        .label(management_locale_text("All", "全选", "全選"))
                        .disabled(pending)
                        .on_click(cx.listener({
                            let provider_ids = provider_ids.to_vec();
                            let route_ids = route_ids.to_vec();
                            move |this, _, _, cx| {
                                this.toggle_workspace_select_all(&provider_ids, &route_ids, cx)
                            }
                        })),
                )
            })
            .child(
                Button::new("management-workspace-batch-enable")
                    .xsmall()
                    .ghost()
                    .label(WorkspaceBatchAction::Enable.label())
                    .disabled(pending || count == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.apply_workspace_batch(WorkspaceBatchAction::Enable, cx)
                    })),
            )
            .child(
                Button::new("management-workspace-batch-disable")
                    .xsmall()
                    .ghost()
                    .label(WorkspaceBatchAction::Disable.label())
                    .disabled(pending || count == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.apply_workspace_batch(WorkspaceBatchAction::Disable, cx)
                    })),
            )
            .child(
                Button::new("management-workspace-batch-delete")
                    .xsmall()
                    .ghost()
                    .label(WorkspaceBatchAction::Delete.label())
                    .disabled(pending || count == 0)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.confirm_workspace_batch_delete(window, cx)
                    })),
            )
            .child(
                Button::new("management-workspace-batch-done")
                    .xsmall()
                    .ghost()
                    .label(management_locale_text("Done", "完成", "完成"))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_workspace_selecting(cx))),
            )
            .into_any_element()
    }

    /// Confirms removing every selected record before the writes start.
    fn confirm_workspace_batch_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace_selection_count() == 0 || self.mutation.is_some() {
            return;
        }
        let count = self.workspace_selection_count();
        let entity = cx.weak_entity();
        let active_locale = locale::current_locale();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let entity = entity.clone();
            alert
                .title(match active_locale {
                    ResolvedLocale::En => format!("Delete {count} selected records?"),
                    ResolvedLocale::ZhCn => format!("删除选中的 {count} 条记录？"),
                    ResolvedLocale::ZhTw => format!("刪除選取的 {count} 筆記錄？"),
                })
                .description(management_locale_text_for(
                    active_locale,
                    "The selected Providers and route groups are removed, and every Agent using them loses those models. This cannot be undone.",
                    "选中的供应商与路由组会被移除，所有使用它们的 Agent 都会失去这些模型。此操作无法撤销。",
                    "選取的供應商與路由組會被移除，所有使用它們的 Agent 都會失去這些模型。此操作無法復原。",
                ))
                .confirm()
                .ok_text(management_locale_text_for(
                    active_locale,
                    "Delete",
                    "删除",
                    "刪除",
                ))
                .cancel_text(management_locale_text_for(
                    active_locale,
                    "Cancel",
                    "取消",
                    "取消",
                ))
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.apply_workspace_batch(WorkspaceBatchAction::Delete, cx);
                    });
                    true
                })
        });
    }

    /// Applies one command to every selected record, in list order.
    ///
    /// The writes run one after another rather than as one authority call: the
    /// contracts are per record, and a batch that reported a single failure for
    /// several records would hide which ones survived. Each success is counted,
    /// and the first failure is reported with how many were skipped.
    pub(super) fn apply_workspace_batch(
        &mut self,
        action: WorkspaceBatchAction,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        if self.mutation.is_some() {
            return;
        }
        let providers = self
            .providers
            .iter()
            .filter(|profile| {
                self.workspace_selected_providers
                    .contains(profile.id.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        let routes = self
            .model_routes
            .iter()
            .filter(|route| self.workspace_selected_routes.contains(route.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let total = providers.len() + routes.len();
        if total == 0 {
            return;
        }
        let active_locale = locale::current_locale();
        self.error = None;
        self.mutation = Some(ManagementMutation::Gateway);
        cx.notify();
        let this = cx.entity();
        self.mutation_task = Some(cx.spawn(async move |_, cx| {
            let mut changed = 0usize;
            let mut failures = Vec::new();
            match action {
                WorkspaceBatchAction::Enable | WorkspaceBatchAction::Disable => {
                    let enabled = action == WorkspaceBatchAction::Enable;
                    for mut profile in providers {
                        profile.status = if enabled {
                            vibex_core::ModelProviderProfileStatus::Enabled
                        } else {
                            vibex_core::ModelProviderProfileStatus::Disabled
                        };
                        let expected_revision = profile.revision;
                        match backend
                            .management()
                            .update_model_provider_profile(MutationRequest::new(
                                vibex_core::ModelProviderProfileUpdateRequest {
                                    profile,
                                    expected_revision,
                                },
                            ))
                            .await
                        {
                            Ok(_) => changed += 1,
                            Err(error) => failures.push((error.code, error.message)),
                        }
                    }
                    for mut route in routes {
                        route.status = if enabled {
                            vibex_core::RouteStatus::Enabled
                        } else {
                            vibex_core::RouteStatus::Disabled
                        };
                        let expected_revision = route.revision;
                        match backend
                            .management()
                            .update_model_route(MutationRequest::new(
                                vibex_core::ModelRouteUpdateRequest {
                                    route,
                                    expected_revision,
                                },
                            ))
                            .await
                        {
                            Ok(_) => changed += 1,
                            Err(error) => failures.push((error.code, error.message)),
                        }
                    }
                    this.update(cx, |this, cx| {
                        this.mutation = None;
                        match failures.first() {
                            Some((code, message)) => {
                                this.error = Some(match active_locale {
                                    ResolvedLocale::En => format!(
                                        "{code}: {message} ({} of {total} changed)",
                                        changed
                                    ),
                                    ResolvedLocale::ZhCn => {
                                        format!("{code}：{message}（已修改 {changed}/{total} 条）")
                                    }
                                    ResolvedLocale::ZhTw => {
                                        format!("{code}：{message}（已修改 {changed}/{total} 筆）")
                                    }
                                });
                            }
                            None => {
                                this.notice = Some(match active_locale {
                                    ResolvedLocale::En => {
                                        format!("Updated {changed} record(s)")
                                    }
                                    ResolvedLocale::ZhCn => {
                                        format!("已更新 {changed} 条记录")
                                    }
                                    ResolvedLocale::ZhTw => {
                                        format!("已更新 {changed} 筆記錄")
                                    }
                                });
                            }
                        }
                        cx.notify();
                        this.refresh(cx);
                    });
                }
                WorkspaceBatchAction::Delete => {
                    for route in routes {
                        match backend
                            .management()
                            .delete_model_route(MutationRequest::new(
                                vibex_core::ModelRouteDeleteRequest {
                                    model_route_id: route.id,
                                },
                            ))
                            .await
                        {
                            Ok(_) => changed += 1,
                            Err(error) => failures.push((error.code, error.message)),
                        }
                    }
                    for profile in providers {
                        match backend
                            .management()
                            .delete_model_provider_profile(MutationRequest::new(profile.id))
                            .await
                        {
                            Ok(()) => changed += 1,
                            Err(error) => failures.push((error.code, error.message)),
                        }
                    }
                    this.update(cx, |this, cx| {
                        this.mutation = None;
                        this.workspace_selected_providers.clear();
                        this.workspace_selected_routes.clear();
                        match failures.first() {
                            Some((code, message)) => {
                                this.error = Some(match active_locale {
                                    ResolvedLocale::En => format!(
                                        "{code}: {message} ({} of {total} deleted)",
                                        changed
                                    ),
                                    ResolvedLocale::ZhCn => {
                                        format!("{code}：{message}（已删除 {changed}/{total} 条）")
                                    }
                                    ResolvedLocale::ZhTw => {
                                        format!("{code}：{message}（已刪除 {changed}/{total} 筆）")
                                    }
                                });
                            }
                            None => {
                                this.notice = Some(match active_locale {
                                    ResolvedLocale::En => {
                                        format!("Deleted {changed} record(s)")
                                    }
                                    ResolvedLocale::ZhCn => {
                                        format!("已删除 {changed} 条记录")
                                    }
                                    ResolvedLocale::ZhTw => {
                                        format!("已刪除 {changed} 筆記錄")
                                    }
                                });
                            }
                        }
                        cx.notify();
                        this.refresh(cx);
                    });
                }
            }
        }));
    }
}

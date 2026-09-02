//! Native local-history picker.
//!
//! The picker is deliberately a thin GPUI projection over the provider-neutral
//! scan/import contract in `vibex-agent`. It owns only filters and selection;
//! the runtime re-reads selected files and persists the authoritative timeline.

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, IntoElement, SharedString, Subscription, Task,
    WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    spinner::Spinner,
    switch::Switch,
    v_flex,
};
use vibex_core::{
    LocalHistoryImportResult, LocalHistoryImportStatus, LocalHistoryScanFolder,
    LocalHistoryScanResult, LocalHistoryScanSession, LocalHistorySelection, LocalHistorySource,
    unix_timestamp_ms,
};
use vibex_desktop_model::LocaleMode;
use vibex_desktop_runtime::DesktopRuntime;

use crate::app::VibexWorkbench;
use crate::assets::agent_brand_icon;
use crate::locale::{self, ResolvedLocale};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportPhase {
    Scanning,
    Ready,
    Importing,
    Done,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderSelection {
    None,
    Some,
    All,
}

#[derive(Clone, Copy)]
struct ImportText {
    search: &'static str,
    scanning: &'static str,
    scanning_hint: &'static str,
    empty: &'static str,
    empty_hint: &'static str,
    no_matches: &'static str,
    scan_failed: &'static str,
    import_failed: &'static str,
    retry: &'static str,
    rescan: &'static str,
    all_agents: &'static str,
    only_importable: &'static str,
    select_all: &'static str,
    clear: &'static str,
    expand_all: &'static str,
    collapse_all: &'static str,
    selected: &'static str,
    summary: &'static str,
    importable: &'static str,
    folders: &'static str,
    no_folder: &'static str,
    imported: &'static str,
    already_imported: &'static str,
    not_found: &'static str,
    failed: &'static str,
    done: &'static str,
    continue_import: &'static str,
    close: &'static str,
    import_selected: &'static str,
    importing: &'static str,
    status_imported: &'static str,
    status_deleted: &'static str,
}

fn text(locale: ResolvedLocale) -> ImportText {
    match locale {
        ResolvedLocale::En => ImportText {
            search: "Search sessions or project directories",
            scanning: "Scanning local Agent sessions...",
            scanning_hint: "Walking each local session store. Large histories may take a moment.",
            empty: "No local sessions found",
            empty_hint: "Run an Agent session first, then scan again.",
            no_matches: "No sessions match the current filters",
            scan_failed: "Scan failed",
            import_failed: "Import failed",
            retry: "Retry",
            rescan: "Rescan",
            all_agents: "All Agents",
            only_importable: "Only importable",
            select_all: "Select all",
            clear: "Clear",
            expand_all: "Expand all",
            collapse_all: "Collapse all",
            selected: "selected",
            summary: "sessions",
            importable: "importable",
            folders: "folders",
            no_folder: "without a workspace",
            imported: "Imported",
            already_imported: "Already imported",
            not_found: "Not found",
            failed: "Failed",
            done: "Import complete",
            continue_import: "Continue importing",
            close: "Close",
            import_selected: "Import selected",
            importing: "Importing...",
            status_imported: "imported",
            status_deleted: "deleted",
        },
        ResolvedLocale::ZhCn => ImportText {
            search: "搜索会话或项目目录",
            scanning: "正在扫描本地 Agent 会话...",
            scanning_hint: "正在遍历各 Agent 的本地会话存储，大型历史记录可能需要一些时间。",
            empty: "未找到本地会话",
            empty_hint: "先运行一次 Agent 会话，然后重新扫描。",
            no_matches: "没有匹配当前筛选条件的会话",
            scan_failed: "扫描失败",
            import_failed: "导入失败",
            retry: "重试",
            rescan: "重新扫描",
            all_agents: "全部 Agent",
            only_importable: "仅显示可导入",
            select_all: "全选",
            clear: "清空",
            expand_all: "全部展开",
            collapse_all: "全部折叠",
            selected: "已选",
            summary: "个会话",
            importable: "可导入",
            folders: "个工作区",
            no_folder: "个无工作区",
            imported: "已导入",
            already_imported: "已存在",
            not_found: "已消失",
            failed: "失败",
            done: "导入完成",
            continue_import: "继续导入",
            close: "关闭",
            import_selected: "导入选中项",
            importing: "正在导入...",
            status_imported: "已导入",
            status_deleted: "已删除",
        },
        ResolvedLocale::ZhTw => ImportText {
            search: "搜尋會話或專案目錄",
            scanning: "正在掃描本機 Agent 會話...",
            scanning_hint: "正在遍歷各 Agent 的本機會話儲存，大型歷史記錄可能需要一些時間。",
            empty: "找不到本機會話",
            empty_hint: "先執行一次 Agent 會話，然後重新掃描。",
            no_matches: "沒有符合目前篩選條件的會話",
            scan_failed: "掃描失敗",
            import_failed: "匯入失敗",
            retry: "重試",
            rescan: "重新掃描",
            all_agents: "全部 Agent",
            only_importable: "僅顯示可匯入",
            select_all: "全選",
            clear: "清除",
            expand_all: "全部展開",
            collapse_all: "全部摺疊",
            selected: "已選",
            summary: "個會話",
            importable: "可匯入",
            folders: "個工作區",
            no_folder: "個無工作區",
            imported: "已匯入",
            already_imported: "已存在",
            not_found: "已消失",
            failed: "失敗",
            done: "匯入完成",
            continue_import: "繼續匯入",
            close: "關閉",
            import_selected: "匯入選取項",
            importing: "正在匯入...",
            status_imported: "已匯入",
            status_deleted: "已刪除",
        },
    }
}

pub struct LocalHistoryImportDialog {
    runtime: Option<Arc<DesktopRuntime>>,
    workbench: WeakEntity<VibexWorkbench>,
    locale_mode: LocaleMode,
    focus_workspace: Option<String>,
    search_input: Entity<InputState>,
    scan: Option<LocalHistoryScanResult>,
    selected: HashSet<LocalHistorySelection>,
    collapsed: HashSet<String>,
    active_source: Option<LocalHistorySource>,
    only_importable: bool,
    phase: ImportPhase,
    scan_error: Option<String>,
    import_error: Option<String>,
    import_result: Option<LocalHistoryImportResult>,
    scan_task: Option<Task<()>>,
    import_task: Option<Task<()>>,
    focus_applied: bool,
    _subscriptions: Vec<Subscription>,
}

impl LocalHistoryImportDialog {
    pub fn new(
        runtime: Option<Arc<DesktopRuntime>>,
        workbench: WeakEntity<VibexWorkbench>,
        focus_workspace: Option<String>,
        locale_mode: LocaleMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let locale = locale::resolve_locale(locale_mode, locale::system_locale().as_deref());
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(text(locale).search));
        let mut dialog = Self {
            runtime,
            workbench,
            locale_mode,
            focus_workspace,
            search_input: input.clone(),
            scan: None,
            selected: HashSet::new(),
            collapsed: HashSet::new(),
            active_source: None,
            only_importable: false,
            phase: ImportPhase::Scanning,
            scan_error: None,
            import_error: None,
            import_result: None,
            scan_task: None,
            import_task: None,
            focus_applied: false,
            _subscriptions: vec![cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })],
        };
        dialog.scan_sessions(true, cx);
        dialog
    }

    fn locale(&self) -> ResolvedLocale {
        locale::resolve_locale(self.locale_mode, locale::system_locale().as_deref())
    }

    fn scan_sessions(&mut self, apply_focus: bool, cx: &mut Context<Self>) {
        if self.phase == ImportPhase::Importing {
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            self.phase = ImportPhase::Error;
            self.scan_error = Some("Agent runtime is not ready".to_string());
            cx.notify();
            return;
        };
        self.phase = ImportPhase::Scanning;
        self.scan_error = None;
        self.import_error = None;
        self.import_result = None;
        self.scan = None;
        self.selected.clear();
        self.collapsed.clear();
        self.active_source = None;
        let focus_workspace = self.focus_workspace.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime.agent().manager().scan_local_history().await
        });
        self.scan_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.scan_task = None;
                match outcome {
                    Ok(Ok(scan)) => {
                        this.phase = ImportPhase::Ready;
                        if apply_focus && !this.focus_applied {
                            this.focus_applied = true;
                            if let Some(target) = focus_workspace.as_deref() {
                                if let Some(folder) = scan
                                    .folders
                                    .iter()
                                    .find(|folder| paths_equal(&folder.workspace_root, target))
                                {
                                    this.selected.extend(folder.sessions.iter().filter_map(
                                        |session| {
                                            (session.status == LocalHistoryImportStatus::New)
                                                .then(|| session.summary.key.clone().into())
                                        },
                                    ));
                                }
                            }
                        }
                        this.scan = Some(scan);
                    }
                    Ok(Err(error)) => {
                        this.phase = ImportPhase::Error;
                        this.scan_error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.phase = ImportPhase::Error;
                        this.scan_error = Some(format!("scan failed: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn visible_folders(
        &self,
        cx: &App,
    ) -> Vec<(LocalHistoryScanFolder, Vec<LocalHistoryScanSession>)> {
        let query = self.search_input.read(cx).value().trim().to_lowercase();
        let mut folders = Vec::new();
        let Some(scan) = self.scan.as_ref() else {
            return folders;
        };
        for folder in &scan.folders {
            if self
                .active_source
                .is_some_and(|source| !folder.sources.contains(&source))
            {
                continue;
            }
            let mut sessions = folder
                .sessions
                .iter()
                .filter(|session| {
                    self.active_source
                        .is_none_or(|source| session.summary.key.source == source)
                })
                .filter(|session| {
                    !self.only_importable || session.status == LocalHistoryImportStatus::New
                })
                .filter(|session| {
                    query.is_empty()
                        || folder.workspace_root.to_lowercase().contains(&query)
                        || session.summary.title.to_lowercase().contains(&query)
                        || session
                            .summary
                            .key
                            .external_id
                            .to_lowercase()
                            .contains(&query)
                        || session
                            .summary
                            .key
                            .source
                            .label()
                            .to_lowercase()
                            .contains(&query)
                        || session
                            .summary
                            .model
                            .as_deref()
                            .is_some_and(|model| model.to_lowercase().contains(&query))
                })
                .cloned()
                .collect::<Vec<_>>();
            sessions.sort_by(|left, right| {
                right
                    .summary
                    .updated_at_ms
                    .or(right.summary.started_at_ms)
                    .cmp(&left.summary.updated_at_ms.or(left.summary.started_at_ms))
            });
            if !sessions.is_empty() {
                folders.push((folder.clone(), sessions));
            }
        }
        folders
    }

    fn folder_selection(&self, sessions: &[LocalHistoryScanSession]) -> FolderSelection {
        let selectable = sessions
            .iter()
            .filter(|session| session.status == LocalHistoryImportStatus::New)
            .collect::<Vec<_>>();
        let selected = selectable
            .iter()
            .filter(|session| self.selected.contains(&session.summary.key.clone().into()))
            .count();
        if !selectable.is_empty() && selected == selectable.len() {
            FolderSelection::All
        } else if selected > 0 {
            FolderSelection::Some
        } else {
            FolderSelection::None
        }
    }

    fn toggle_session(&mut self, key: LocalHistorySelection, cx: &mut Context<Self>) {
        if self.phase != ImportPhase::Ready {
            return;
        }
        if !self.selected.remove(&key) {
            self.selected.insert(key);
        }
        cx.notify();
    }

    fn toggle_folder(&mut self, sessions: &[LocalHistoryScanSession], cx: &mut Context<Self>) {
        if self.phase != ImportPhase::Ready {
            return;
        }
        let keys = sessions
            .iter()
            .filter(|session| session.status == LocalHistoryImportStatus::New)
            .map(|session| session.summary.key.clone().into())
            .collect::<Vec<LocalHistorySelection>>();
        let all_selected = keys.iter().all(|key| self.selected.contains(key));
        for key in keys {
            if all_selected {
                self.selected.remove(&key);
            } else {
                self.selected.insert(key);
            }
        }
        cx.notify();
    }

    fn toggle_collapse(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&path) {
            self.collapsed.insert(path);
        }
        cx.notify();
    }

    fn toggle_collapse_all(
        &mut self,
        folders: &[(LocalHistoryScanFolder, Vec<LocalHistoryScanSession>)],
        cx: &mut Context<Self>,
    ) {
        let all_collapsed = !folders.is_empty()
            && folders
                .iter()
                .all(|(folder, _)| self.collapsed.contains(&folder.workspace_root));
        for (folder, _) in folders {
            if all_collapsed {
                self.collapsed.remove(&folder.workspace_root);
            } else {
                self.collapsed.insert(folder.workspace_root.clone());
            }
        }
        cx.notify();
    }

    fn import_selected(&mut self, cx: &mut Context<Self>) {
        if self.phase != ImportPhase::Ready || self.selected.is_empty() {
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            self.import_error = Some("Agent runtime is not ready".to_string());
            cx.notify();
            return;
        };
        self.phase = ImportPhase::Importing;
        self.import_error = None;
        let selections = self.selected.iter().cloned().collect::<Vec<_>>();
        let workbench = self.workbench.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            runtime
                .agent()
                .manager()
                .import_local_history(selections)
                .await
        });
        self.import_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                this.import_task = None;
                match outcome {
                    Ok(Ok(result)) => {
                        let selected_session =
                            result.sessions.first().map(|session| session.id.clone());
                        this.import_result = Some(result);
                        this.phase = ImportPhase::Done;
                        cx.defer(move |cx| {
                            let _ = workbench.update(cx, |workbench, cx| {
                                workbench.complete_local_history_import(selected_session, cx);
                            });
                        });
                    }
                    Ok(Err(error)) => {
                        this.phase = ImportPhase::Ready;
                        this.import_error = Some(format!("{}: {}", error.code, error.message));
                    }
                    Err(error) => {
                        this.phase = ImportPhase::Ready;
                        this.import_error = Some(format!("import failed: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn checkbox(
        &self,
        id: SharedString,
        checked: bool,
        partial: bool,
        disabled: bool,
        on_toggle: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut box_view = div()
            .id(id)
            .size(px(16.0))
            .flex_none()
            .rounded(px(4.0))
            .border_1()
            .border_color(cx.theme().border)
            .flex()
            .items_center()
            .justify_center();
        if checked {
            box_view = box_view
                .bg(cx.theme().primary)
                .border_color(cx.theme().primary)
                .child(
                    Icon::new(IconName::Check)
                        .size(px(12.0))
                        .text_color(cx.theme().primary_foreground),
                );
        } else if partial {
            box_view = box_view.child(
                Icon::new(IconName::Minus)
                    .size(px(12.0))
                    .text_color(cx.theme().muted_foreground),
            );
        }
        if disabled {
            box_view = box_view.opacity(0.5);
        } else {
            box_view = box_view
                .cursor_pointer()
                .hover(|style| style.border_color(cx.theme().primary))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| on_toggle(this, cx)));
        }
        box_view.into_any_element()
    }

    fn render_folder(
        &self,
        folder: &LocalHistoryScanFolder,
        sessions: &[LocalHistoryScanSession],
        pending: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let collapsed = self.collapsed.contains(&folder.workspace_root);
        let selection = self.folder_selection(sessions);
        let selectable_count = sessions
            .iter()
            .filter(|session| session.status == LocalHistoryImportStatus::New)
            .count();
        let selected_count = sessions
            .iter()
            .filter(|session| {
                session.status == LocalHistoryImportStatus::New
                    && self.selected.contains(&session.summary.key.clone().into())
            })
            .count();
        let folder_path = folder.workspace_root.clone();
        let mut result =
            vec![
                h_flex()
                    .id(SharedString::from(format!(
                        "local-history-folder:{folder_path}"
                    )))
                    .w_full()
                    .min_w_0()
                    .h(px(36.0))
                    .items_center()
                    .gap_2()
                    .px_2()
                    .rounded(px(6.0))
                    .bg(cx.theme().muted.opacity(0.24))
                    .child(self.checkbox(
                        SharedString::from(format!("local-history-folder-check:{folder_path}")),
                        selection == FolderSelection::All,
                        selection == FolderSelection::Some,
                        pending || selectable_count == 0,
                        {
                            let sessions = sessions.to_vec();
                            move |this, cx| this.toggle_folder(&sessions, cx)
                        },
                        cx,
                    ))
                    .child(
                        Button::new(SharedString::from(format!(
                            "local-history-folder-toggle:{folder_path}"
                        )))
                        .ghost()
                        .xsmall()
                        .icon(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .disabled(pending)
                        .on_click(cx.listener({
                            let folder_path = folder_path.clone();
                            move |this, _: &ClickEvent, _, cx| {
                                this.toggle_collapse(folder_path.clone(), cx)
                            }
                        })),
                    )
                    .child(
                        Icon::new(if collapsed {
                            IconName::Folder
                        } else {
                            IconName::FolderOpen
                        })
                        .size(px(15.0))
                        .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_medium()
                                    .child(folder_name(&folder.workspace_root)),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(compact_path(&folder.workspace_root)),
                            ),
                    )
                    .child(h_flex().flex_none().gap_1().children(
                        folder.sources.iter().take(4).map(|source| {
                            agent_brand_icon(source.agent_id().as_str(), px(15.0), None)
                        }),
                    ))
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{selected_count}/{selectable_count}")),
                    )
                    .into_any_element(),
            ];
        if !collapsed {
            result.extend(
                sessions
                    .iter()
                    .map(|session| self.render_session(session, pending, cx)),
            );
        }
        result
    }

    fn render_session(
        &self,
        session: &LocalHistoryScanSession,
        pending: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let summary = &session.summary;
        let key: LocalHistorySelection = summary.key.clone().into();
        let selectable = session.status == LocalHistoryImportStatus::New && !pending;
        let checked = self.selected.contains(&key);
        let title = if summary.title.trim().is_empty() {
            summary.key.source.label()
        } else {
            &summary.title
        };
        let key_for_click = key.clone();
        h_flex()
            .id(SharedString::from(format!(
                "local-history-session:{}:{}",
                summary.key.source.key(),
                summary.key.external_id
            )))
            .w_full()
            .min_w_0()
            .h(px(36.0))
            .items_center()
            .gap_2()
            .px_2()
            .pl(px(38.0))
            .rounded(px(6.0))
            .when(selectable, |row| {
                row.cursor_pointer()
                    .hover(|style| style.bg(cx.theme().muted.opacity(0.36)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_session(key_for_click.clone(), cx)
                    }))
            })
            .when(!selectable, |row| row.opacity(0.62))
            .child(self.checkbox(
                SharedString::from(format!(
                    "local-history-session-check:{}:{}",
                    summary.key.source.key(),
                    summary.key.external_id
                )),
                checked,
                false,
                !selectable,
                {
                    let key = key.clone();
                    move |this, cx| this.toggle_session(key.clone(), cx)
                },
                cx,
            ))
            .child(agent_brand_icon(
                summary.key.source.agent_id().as_str(),
                px(16.0),
                None,
            ))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_sm()
                    .child(title.to_string()),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary.key.source.label()),
            )
            .when(session.status_is_imported(), |row| {
                row.child(status_badge("imported", self.locale(), cx))
            })
            .when(session.status_is_deleted(), |row| {
                row.child(status_badge("deleted", self.locale(), cx))
            })
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} {}",
                        summary.message_count,
                        text(self.locale()).summary
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(42.0))
                    .text_right()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(relative_time(
                        summary.updated_at_ms.or(summary.started_at_ms),
                        self.locale(),
                    )),
            )
            .into_any_element()
    }
}

impl Render for LocalHistoryImportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locale = self.locale();
        let strings = text(locale);
        match self.phase {
            ImportPhase::Scanning => {
                return v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .p_8()
                    .child(Spinner::new())
                    .child(div().text_sm().font_medium().child(strings.scanning))
                    .child(
                        div()
                            .max_w(px(360.0))
                            .text_center()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(strings.scanning_hint),
                    );
            }
            ImportPhase::Error => {
                return v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .p_8()
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .size(px(26.0))
                            .text_color(cx.theme().danger),
                    )
                    .child(div().text_sm().font_medium().child(strings.scan_failed))
                    .when_some(self.scan_error.clone(), |view, error| {
                        view.child(
                            div()
                                .max_w(px(420.0))
                                .text_xs()
                                .text_center()
                                .text_color(cx.theme().muted_foreground)
                                .child(error),
                        )
                    })
                    .child(
                        Button::new("local-history-retry")
                            .small()
                            .label(strings.retry)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.scan_sessions(true, cx)
                            })),
                    );
            }
            ImportPhase::Done => {
                let result = self.import_result.as_ref();
                let imported = result.map_or(0, |result| result.sessions.len() as u32);
                let already = result.map_or(0, |result| result.already_imported);
                let not_found = result.map_or(0, |result| result.not_found);
                let failed = result.map_or(0, |result| result.failed);
                let errors = result
                    .map(|result| result.errors.clone())
                    .unwrap_or_default();
                return v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .p_8()
                    .child(
                        Icon::new(if failed == 0 {
                            IconName::CircleCheck
                        } else {
                            IconName::TriangleAlert
                        })
                        .size(px(28.0))
                        .text_color(if failed == 0 {
                            cx.theme().success
                        } else {
                            cx.theme().danger
                        }),
                    )
                    .child(div().text_sm().font_medium().child(strings.done))
                    .child(h_flex().gap_5().children([
                        stat(strings.imported, imported),
                        stat(strings.already_imported, already),
                        stat(strings.not_found, not_found),
                        stat(strings.failed, failed),
                    ]))
                    .when(!errors.is_empty(), |view| {
                        view.child(
                            v_flex()
                                .max_h(px(96.0))
                                .w_full()
                                .max_w(px(520.0))
                                .overflow_y_scrollbar()
                                .gap_1()
                                .p_2()
                                .border_1()
                                .border_color(cx.theme().danger.opacity(0.4))
                                .children(errors.into_iter().map(|error| {
                                    div().text_xs().text_color(cx.theme().danger).child(error)
                                })),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("local-history-continue")
                                    .small()
                                    .outline()
                                    .label(strings.continue_import)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.import_result = None;
                                        this.focus_applied = true;
                                        this.scan_sessions(false, cx);
                                    })),
                            )
                            .child(
                                Button::new("local-history-done-close")
                                    .small()
                                    .primary()
                                    .label(strings.close)
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            ),
                    );
            }
            ImportPhase::Ready | ImportPhase::Importing => {}
        }

        let pending = self.phase == ImportPhase::Importing;
        let folders = self.visible_folders(cx);
        let selected_count = self.selected.len();
        let all_importable = folders
            .iter()
            .flat_map(|(_, sessions)| sessions.iter())
            .filter(|session| session.status == LocalHistoryImportStatus::New)
            .count();
        let all_collapsed = !folders.is_empty()
            && folders
                .iter()
                .all(|(folder, _)| self.collapsed.contains(&folder.workspace_root));
        let present_sources = self
            .scan
            .as_ref()
            .map(|scan| {
                scan.folders
                    .iter()
                    .flat_map(|folder| folder.sources.iter().copied())
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let body_height = (f32::from(window.viewport_size().height) - 180.0).clamp(220.0, 560.0);
        let list = if folders.is_empty() {
            let empty = self
                .scan
                .as_ref()
                .is_none_or(|scan| scan.folders.is_empty());
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .p_8()
                .child(
                    Icon::new(IconName::FolderOpen)
                        .size(px(26.0))
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().text_sm().child(if empty {
                    strings.empty
                } else {
                    strings.no_matches
                }))
                .when(empty, |view| {
                    view.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(strings.empty_hint),
                    )
                })
                .into_any_element()
        } else {
            let rows = folders
                .iter()
                .flat_map(|(folder, sessions)| self.render_folder(folder, sessions, pending, cx))
                .collect::<Vec<_>>();
            v_flex()
                .id("local-history-list")
                .flex_1()
                .min_h_0()
                .max_h(px(body_height))
                .overflow_y_scrollbar()
                .gap_1()
                .p_1()
                .children(rows)
                .into_any_element()
        };

        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Input::new(&self.search_input)
                            .flex_1()
                            .min_w(px(180.0))
                            .rounded(px(8.0))
                            .bg(cx.theme().background),
                    )
                    .child(
                        Switch::new("local-history-only-importable")
                            .small()
                            .checked(self.only_importable)
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.only_importable = *checked;
                                cx.notify();
                            }))
                            .tooltip(strings.only_importable),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(strings.only_importable),
                    )
                    .child(
                        Button::new("local-history-collapse-all")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronsUpDown)
                            .tooltip(if all_collapsed {
                                strings.expand_all
                            } else {
                                strings.collapse_all
                            })
                            .disabled(pending || folders.is_empty())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.toggle_collapse_all(&folders, cx)
                            })),
                    )
                    .child(
                        Button::new("local-history-select-all")
                            .ghost()
                            .small()
                            .label(strings.select_all)
                            .disabled(pending || all_importable == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let keys = this
                                    .visible_folders(cx)
                                    .into_iter()
                                    .flat_map(|(_, sessions)| sessions.into_iter())
                                    .filter(|session| {
                                        session.status == LocalHistoryImportStatus::New
                                    })
                                    .map(|session| session.summary.key.into())
                                    .collect::<Vec<LocalHistorySelection>>();
                                this.selected.extend(keys);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("local-history-clear")
                            .ghost()
                            .small()
                            .label(strings.clear)
                            .disabled(pending || selected_count == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selected.clear();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("local-history-rescan")
                            .ghost()
                            .small()
                            .icon(IconName::LoaderCircle)
                            .label(strings.rescan)
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, _, cx| this.scan_sessions(false, cx))),
                    ),
            )
            .when(present_sources.len() > 1, |view| {
                view.child(
                    h_flex().flex_wrap().gap_1().children(
                        std::iter::once((None, strings.all_agents.to_string()))
                            .chain(
                                present_sources
                                    .iter()
                                    .copied()
                                    .map(|source| (Some(source), source.label().to_string())),
                            )
                            .map(|(source, label)| {
                                let active = self.active_source == source;
                                Button::new(SharedString::from(format!(
                                    "local-history-source:{}",
                                    source.map_or("all", LocalHistorySource::key)
                                )))
                                .xsmall()
                                .label(label)
                                .when(active, |button| button.secondary())
                                .when(!active, |button| button.ghost())
                                .disabled(pending)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.active_source = source;
                                        cx.notify();
                                    },
                                ))
                            }),
                    ),
                )
            })
            .child(list)
            .when_some(self.scan_error.clone(), |view, error| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("{}: {}", strings.scan_failed, error)),
                )
            })
            .when_some(self.import_error.clone(), |view, error| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("{}: {}", strings.import_failed, error)),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(
                        self.scan
                            .as_ref()
                            .map(|scan| {
                                let summary = div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{} {} · {} {} · {} {}",
                                        scan.total_sessions,
                                        strings.summary,
                                        scan.importable_count,
                                        strings.importable,
                                        scan.folders.len(),
                                        strings.folders
                                    ));
                                summary.when(scan.unassigned_count > 0, |view| {
                                    view.child(format!(
                                        " · {} {}",
                                        scan.unassigned_count, strings.no_folder
                                    ))
                                })
                            })
                            .unwrap_or_else(|| div().flex_1().child("")),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(if selected_count > 0 {
                                cx.theme().foreground
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(format!("{} {}", selected_count, strings.selected)),
                    )
                    .child(
                        Button::new("local-history-close")
                            .ghost()
                            .small()
                            .label(strings.close)
                            .disabled(pending)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("local-history-import")
                            .primary()
                            .small()
                            .loading(pending)
                            .label(if pending {
                                strings.importing
                            } else {
                                strings.import_selected
                            })
                            .disabled(pending || selected_count == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.import_selected(cx))),
                    ),
            )
    }
}

trait ScanSessionStatusExt {
    fn status_is_imported(&self) -> bool;
    fn status_is_deleted(&self) -> bool;
}

impl ScanSessionStatusExt for LocalHistoryScanSession {
    fn status_is_imported(&self) -> bool {
        self.status == LocalHistoryImportStatus::Imported
    }

    fn status_is_deleted(&self) -> bool {
        self.status == LocalHistoryImportStatus::Deleted
    }
}

fn status_badge(
    status: &str,
    locale: ResolvedLocale,
    cx: &Context<LocalHistoryImportDialog>,
) -> AnyElement {
    let strings = text(locale);
    let label = if status == "imported" {
        strings.status_imported
    } else {
        strings.status_deleted
    };
    div()
        .flex_none()
        .rounded(px(5.0))
        .border_1()
        .border_color(cx.theme().border)
        .px(px(5.0))
        .py(px(1.0))
        .text_xs()
        .child(label)
        .into_any_element()
}

fn stat(label: &'static str, value: u32) -> AnyElement {
    v_flex()
        .items_center()
        .gap_1()
        .child(div().text_lg().font_medium().child(value.to_string()))
        .child(div().text_xs().child(label))
        .into_any_element()
}

fn folder_name(path: &str) -> String {
    PathLike::new(path).file_name().unwrap_or(path).to_string()
}

struct PathLike<'a>(&'a str);

impl<'a> PathLike<'a> {
    fn new(path: &'a str) -> Self {
        Self(path)
    }
    fn file_name(&self) -> Option<&'a str> {
        self.0
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .filter(|value| !value.is_empty())
    }
}

fn compact_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy().replace('\\', "/");
        if path == home {
            return "~".to_string();
        }
        if let Some(rest) = path.strip_prefix(&(home + "/")) {
            return format!("~/{rest}");
        }
    }
    path
}

fn paths_equal(left: &str, right: &str) -> bool {
    let normalize = |value: &str| {
        let mut value = value.trim().replace('\\', "/");
        while value.len() > 1 && value.ends_with('/') {
            value.pop();
        }
        value
    };
    let left = normalize(left);
    let right = normalize(right);
    left == right || left.eq_ignore_ascii_case(&right)
}

fn relative_time(timestamp_ms: Option<i64>, locale: ResolvedLocale) -> String {
    let Some(timestamp_ms) = timestamp_ms else {
        return String::new();
    };
    let minutes = ((unix_timestamp_ms() - timestamp_ms).max(0) / 60_000) as u64;
    match locale {
        ResolvedLocale::En => {
            if minutes < 1 {
                "now".to_string()
            } else if minutes < 60 {
                format!("{minutes}m")
            } else if minutes < 1_440 {
                format!("{}h", minutes / 60)
            } else {
                format!("{}d", minutes / 1_440)
            }
        }
        ResolvedLocale::ZhCn => {
            if minutes < 1 {
                "刚刚".to_string()
            } else if minutes < 60 {
                format!("{minutes}分钟前")
            } else if minutes < 1_440 {
                format!("{}小时前", minutes / 60)
            } else {
                format!("{}天前", minutes / 1_440)
            }
        }
        ResolvedLocale::ZhTw => {
            if minutes < 1 {
                "剛剛".to_string()
            } else if minutes < 60 {
                format!("{minutes} 分鐘前")
            } else if minutes < 1_440 {
                format!("{} 小時前", minutes / 60)
            } else {
                format!("{} 天前", minutes / 1_440)
            }
        }
    }
}

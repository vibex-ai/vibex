//! Compact team view. Lists remain virtualized; actions refer to the task the
//! row shows and replies are fenced to their backend and selected session.

use super::*;
use vibex_core::{
    AgentDelegationId, DelegationTaskPhase, DelegationTaskView, TeamAcknowledgeRequest,
    TeamTaskAction, TeamTaskControlRequest,
};

impl MobileApp {
    pub(super) fn open_team(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.team_error = None;
        self.team_cancel_confirmation = None;
        let Some(session_id) = self
            .controller
            .as_ref()
            .and_then(|c| c.state.selected_session_id.clone())
        else {
            return;
        };
        if let Some(team) = self.team_controller.as_mut() {
            team.select_session(session_id);
        }
        self.show_overlay(MobileOverlay::Team, window, cx);
        self.refresh_team(false, cx);
    }

    pub(super) fn refresh_team(&mut self, append: bool, cx: &mut Context<Self>) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let Some(team) = self.team_controller.as_mut() else {
            return;
        };
        let Some(ticket) = team.begin_load(append) else {
            return;
        };
        let runner = gpui_tokio::Tokio::spawn(cx, team.load(&ticket));
        self.tasks
            .push(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
                let result = flatten_join(runner.await);
                let _ = entity.update(cx, |this, cx| {
                    if !this
                        .backend
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &backend))
                    {
                        return;
                    }
                    let reload = this.team_controller.as_mut().is_some_and(|team| {
                        team.apply(&ticket, result) && team.has_pending_refresh()
                    });
                    if reload && this.overlay == Some(MobileOverlay::Team) {
                        this.refresh_team(false, cx);
                    }
                    cx.notify();
                });
            }));
        cx.notify();
    }

    fn control_team(
        &mut self,
        id: AgentDelegationId,
        action: TeamTaskAction,
        cx: &mut Context<Self>,
    ) {
        if self.team_busy {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let Some(team) = self
            .team_controller
            .as_ref()
            .filter(|team| team.is_mutable())
        else {
            return;
        };
        let Some(session_id) = team.session_id().cloned() else {
            return;
        };
        let Some(task) = team.task(&id) else {
            return;
        };
        let request = TeamTaskControlRequest {
            session_id: session_id.clone(),
            task_id: id,
            action,
            expected_revision: Some(task.revision),
        };
        self.team_busy = true;
        self.team_error = None;
        self.team_cancel_confirmation = None;
        let requested_backend = backend.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .control_team_task(MutationRequest::new(request))
                .await
        });
        self.tasks
            .push(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
                let result = flatten_join(runner.await);
                let _ = entity.update(cx, |this, cx| {
                    if !this
                        .backend
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &requested_backend))
                    {
                        return;
                    }
                    this.team_busy = false;
                    if this
                        .team_controller
                        .as_ref()
                        .and_then(|team| team.session_id())
                        != Some(&session_id)
                    {
                        return;
                    }
                    if let Err(error) = result {
                        this.team_error = Some(error.message);
                    }
                    this.refresh_team(false, cx);
                    cx.notify();
                });
            }));
        cx.notify();
    }

    fn acknowledge_team_result(&mut self, task_id: AgentDelegationId, cx: &mut Context<Self>) {
        if self.team_busy {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            return;
        };
        let Some(team) = self
            .team_controller
            .as_ref()
            .filter(|team| team.is_mutable())
        else {
            return;
        };
        let Some(task) = team.task(&task_id) else {
            return;
        };
        let event_ids = team.pending_result_events(task);
        let Some(session_id) = team.session_id().cloned() else {
            return;
        };
        if event_ids.is_empty() {
            return;
        }
        self.team_busy = true;
        self.team_error = None;
        let requested_backend = backend.clone();
        let request = TeamAcknowledgeRequest {
            session_id: session_id.clone(),
            event_ids,
        };
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend
                .acknowledge_team_events(MutationRequest::new(request))
                .await
        });
        self.tasks
            .push(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
                let result = flatten_join(runner.await);
                let _ = entity.update(cx, |this, cx| {
                    if !this
                        .backend
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &requested_backend))
                    {
                        return;
                    }
                    this.team_busy = false;
                    if this
                        .team_controller
                        .as_ref()
                        .and_then(|team| team.session_id())
                        != Some(&session_id)
                    {
                        return;
                    }
                    if let Err(error) = result {
                        this.team_error = Some(error.message);
                    }
                    this.refresh_team(false, cx);
                    cx.notify();
                });
            }));
        cx.notify();
    }

    pub(super) fn render_team(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let team = self.team_controller.as_ref();
        let loading = team.is_some_and(|team| team.state().is_loading());
        let count = team
            .and_then(|team| team.snapshot())
            .map_or(0, |s| s.tasks.len());
        let refresh = Button::new("team-refresh")
            .ghost()
            .h_10()
            .disabled(loading || self.team_busy)
            .label(locale::common("Refresh"))
            .on_click(cx.listener(|this, _, _, cx| this.refresh_team(false, cx)))
            .into_any_element();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::bg_primary())
            .child(self.render_overlay_header(
                "mobile-team",
                locale::text("Team", "团队", "團隊"),
                Some(refresh),
                cx,
            ))
            .children(self.team_error.as_ref().map(|error| {
                div()
                    .px_4()
                    .py_2()
                    .text_sm()
                    .text_color(theme::accent_red())
                    .child(error.clone())
            }))
            .when(count == 0, |root| {
                root.child(
                    div().p_4().text_sm().text_color(theme::text_muted()).child(
                        team.map(|team| team.status_label(locale::current()))
                            .unwrap_or(locale::text(
                                "Team views unavailable",
                                "团队视图不可用",
                                "團隊檢視無法使用",
                            )),
                    ),
                )
            })
            .when(count > 0, |root| {
                root.child(
                    uniform_list(
                        "mobile-team-tasks",
                        count,
                        cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                            range
                                .filter_map(|ix| {
                                    this.team_controller
                                        .as_ref()?
                                        .snapshot()?
                                        .tasks
                                        .get(ix)
                                        .cloned()
                                })
                                .map(|task| this.render_team_task(&task, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.team_scroll)
                    .flex_1()
                    .min_h_0(),
                )
            })
            .when(
                team.is_some_and(|team| team.state().phase == AsyncPhase::Failed) && count > 0,
                |root| {
                    root.child(
                        div()
                            .px_4()
                            .py_2()
                            .text_sm()
                            .text_color(theme::accent_red())
                            .child(locale::text(
                                "Couldn’t refresh team",
                                "无法刷新团队",
                                "無法重新整理團隊",
                            )),
                    )
                },
            )
            .when(team.is_some_and(|team| !team.is_mutable()), |root| {
                root.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(locale::text("Read only", "只读", "唯讀")),
                )
            })
            .when(team.is_some_and(|team| team.has_more()), |root| {
                root.child(
                    Button::new("team-load-more")
                        .ghost()
                        .h_10()
                        .label(locale::text("Load more", "加载更多", "載入更多"))
                        .disabled(loading)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_team(true, cx))),
                )
            })
            .when(team.is_some_and(|team| team.is_limited()), |root| {
                root.child(div().p_4().text_sm().child(locale::text(
                    "Showing the first 1,000 team records",
                    "显示前 1,000 条团队记录",
                    "顯示前 1,000 筆團隊記錄",
                )))
            })
            .into_any_element()
    }

    fn render_team_task(
        &self,
        task: &DelegationTaskView,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(id) = task.task_ref.task_id() else {
            return div().into_any_element();
        };
        let session_id = task
            .session_ref
            .as_ref()
            .and_then(vibex_core::VibexUseRef::session_id);
        let team = self.team_controller.as_ref();
        let mutable = team.is_some_and(|team| team.is_mutable()) && !self.team_busy;
        let pending = team.is_some_and(|team| !team.pending_result_events(task).is_empty());
        let latest = team.and_then(|team| team.snapshot()).and_then(|snapshot| {
            snapshot
                .executions
                .iter()
                .filter(|execution| {
                    execution.task_ref.as_ref() == Some(&task.task_ref)
                        && task
                            .current_execution_ref
                            .as_ref()
                            .is_none_or(|current| current == &execution.execution_ref)
                })
                .max_by_key(|execution| execution.created_at_ms)
        });
        let summary = latest
            .and_then(|execution| execution.summary.clone())
            .unwrap_or_else(|| task.task_summary.clone());
        let state = vibex_ui::team_task_state_label(task, locale::current());
        let title = task.title.clone();
        let revision = task.revision;
        let confirm =
            self.team_cancel_confirmation
                .as_ref()
                .is_some_and(|(task_id, confirmed_revision)| {
                    task_id == &id && *confirmed_revision == revision
                });
        let accept_id = id.clone();
        let stop_id = id.clone();
        let ack_id = id.clone();
        div()
            .id(format!("team-task-{id}"))
            .h_40()
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_1()
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_sm()
                    .truncate()
                    .child(title),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(if pending {
                        format!(
                            "{state} · {}",
                            locale::text("Result ready", "结果待回收", "結果待回收")
                        )
                    } else {
                        state.to_string()
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_secondary())
                    .truncate()
                    .child(summary),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .mt_auto()
                    .children(session_id.map(|session_id| {
                        Button::new(format!("team-open-{id}"))
                            .ghost()
                            .h_10()
                            .label(locale::text("Open worker", "打开成员", "開啟成員"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.dismiss_overlay(Some(window), cx);
                                this.open_session(session_id.clone(), cx);
                            }))
                    }))
                    .when(mutable && pending && !confirm, |row| {
                        row.child(
                            Button::new(format!("team-collect-{id}"))
                                .ghost()
                                .h_10()
                                .label(locale::text("Mark read", "标为已读", "標為已讀"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.acknowledge_team_result(ack_id.clone(), cx)
                                })),
                        )
                    })
                    .when(
                        mutable && task.phase == DelegationTaskPhase::AwaitingReview && !confirm,
                        |row| {
                            row.child(
                                Button::new(format!("team-accept-{id}"))
                                    .ghost()
                                    .h_10()
                                    .label(locale::text("Accept", "验收", "驗收"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.control_team(
                                            accept_id.clone(),
                                            TeamTaskAction::Accept,
                                            cx,
                                        )
                                    })),
                            )
                        },
                    )
                    .when(
                        mutable
                            && !task.phase.is_terminal()
                            && task.phase != DelegationTaskPhase::Cancelling,
                        |row| {
                            row.child(
                                Button::new(format!("team-stop-{id}"))
                                    .ghost()
                                    .h_10()
                                    .label(if confirm {
                                        locale::text("Stop workers", "停止成员", "停止成員")
                                    } else {
                                        locale::text("Stop…", "停止…", "停止…")
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if confirm {
                                            this.control_team(
                                                stop_id.clone(),
                                                TeamTaskAction::Cancel { cascade: true },
                                                cx,
                                            );
                                        } else {
                                            this.team_cancel_confirmation =
                                                Some((stop_id.clone(), revision));
                                            cx.notify();
                                        }
                                    })),
                            )
                        },
                    )
                    .when(confirm, |row| {
                        row.child(
                            Button::new(format!("team-keep-{id}"))
                                .ghost()
                                .h_10()
                                .label(locale::text("Keep running", "继续运行", "繼續執行"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.team_cancel_confirmation = None;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .into_any_element()
    }
}

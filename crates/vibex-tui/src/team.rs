//! Team tasks in the existing work dock. The dock owns selection; the shared
//! controller owns request generations and the runtime owns task facts.

use crate::app::{App, DockRow, DockSection, Effect};
use crate::reduce::Outcome;
use vibex_core::{
    AgentDelegationId, DelegationTaskPhase, TeamAcknowledgeRequest, TeamTaskAction,
    TeamTaskControlRequest, VibexSessionId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamDockAction {
    None,
    Inspect(AgentDelegationId),
    Open(VibexSessionId),
    ConfirmStop(TeamTaskControlRequest),
    KeepRunning,
    Control(TeamTaskControlRequest),
    Acknowledge(TeamAcknowledgeRequest),
    Refresh,
    More,
}

impl App {
    pub fn load_team(&mut self, append: bool) -> Option<Effect> {
        if !self.page_owns_session() {
            return None;
        }
        let session_id = self.agent.state.selected_session_id.clone()?;
        self.team.select_session(session_id);
        self.team
            .begin_load(append)
            .map(|ticket| Effect::LoadTeam { ticket })
    }

    pub fn team_dock_rows(&self) -> Vec<DockRow> {
        if !self.team.is_supported() || !self.page_owns_session() {
            return Vec::new();
        }
        let locale = self.strings.locale;
        let mut rows = vec![DockRow::Header {
            section: DockSection::Team,
            count: self.team.snapshot().map_or(0, |s| s.tasks.len()),
        }];
        if self.dock_collapsed.contains(&DockSection::Team) {
            return rows;
        }
        let row = |label: String, action| DockRow::Team { label, action };
        let Some(snapshot) = self.team.snapshot() else {
            rows.push(row(
                self.team.status_label(locale).to_string(),
                TeamDockAction::Refresh,
            ));
            return rows;
        };
        if self.team.state().phase == vibex_ui::AsyncPhase::Failed {
            rows.push(row(
                locale
                    .text("Couldn’t refresh team", "无法刷新团队", "無法重新整理團隊")
                    .to_string(),
                TeamDockAction::Refresh,
            ));
        }
        if snapshot.tasks.is_empty() {
            rows.push(row(
                locale
                    .text("No delegated tasks", "暂无委派任务", "尚無委派任務")
                    .to_string(),
                TeamDockAction::None,
            ));
        }
        for task in &snapshot.tasks {
            let Some(id) = task.task_ref.task_id() else {
                continue;
            };
            if self.dock_hide_done && task.phase.is_terminal() {
                continue;
            }
            let pending_events = self.team.pending_result_events(task);
            let state = vibex_ui::team_task_state_label(task, locale);
            let label = if pending_events.is_empty() {
                format!("{} · {state}", task.title)
            } else {
                format!(
                    "{} · {state} · {}",
                    task.title,
                    locale.text("Result ready", "结果待回收", "結果待回收")
                )
            };
            rows.push(row(label, TeamDockAction::Inspect(id.clone())));
            if self.team_detail.as_ref() != Some(&id) {
                continue;
            }
            if let Some(execution) = snapshot
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
            {
                rows.push(row(
                    format!(
                        "  {} · {}",
                        locale.text("Latest round", "最近一轮", "最近一輪"),
                        vibex_ui::team_execution_outcome_label(execution.outcome, locale)
                    ),
                    TeamDockAction::None,
                ));
                if let Some(summary) = &execution.summary {
                    rows.push(row(format!("  {summary}"), TeamDockAction::None));
                }
            }
            if let Some(session_id) = task
                .session_ref
                .as_ref()
                .and_then(vibex_core::VibexUseRef::session_id)
            {
                rows.push(row(
                    locale
                        .text("  Open worker", "  打开成员", "  開啟成員")
                        .to_string(),
                    TeamDockAction::Open(session_id),
                ));
            }
            if !self.team.is_mutable() || self.team_busy {
                continue;
            }
            let Some(session_id) = self.team.session_id().cloned() else {
                continue;
            };
            if !pending_events.is_empty() {
                rows.push(row(
                    locale
                        .text("  Mark result read", "  标记结果已读", "  標記結果已讀")
                        .to_string(),
                    TeamDockAction::Acknowledge(TeamAcknowledgeRequest {
                        session_id: session_id.clone(),
                        event_ids: pending_events,
                    }),
                ));
            }
            if task.phase == DelegationTaskPhase::AwaitingReview {
                rows.push(row(
                    locale
                        .text("  Accept result", "  验收结果", "  驗收結果")
                        .to_string(),
                    TeamDockAction::Control(TeamTaskControlRequest {
                        session_id: session_id.clone(),
                        task_id: id.clone(),
                        action: TeamTaskAction::Accept,
                        expected_revision: Some(task.revision),
                    }),
                ));
            }
            if !task.phase.is_terminal() && task.phase != DelegationTaskPhase::Cancelling {
                let request = TeamTaskControlRequest {
                    session_id,
                    task_id: id.clone(),
                    action: TeamTaskAction::Cancel { cascade: true },
                    expected_revision: Some(task.revision),
                };
                let confirmed = self.team_confirm_cancel.as_ref() == Some(&request);
                let action = if confirmed {
                    TeamDockAction::Control(request)
                } else {
                    TeamDockAction::ConfirmStop(request)
                };
                let label = if confirmed {
                    locale.text(
                        "  Confirm stop task and workers",
                        "  确认停止任务及成员",
                        "  確認停止任務及成員",
                    )
                } else {
                    locale.text(
                        "  Stop task and workers…",
                        "  停止任务及成员…",
                        "  停止任務及成員…",
                    )
                };
                rows.push(row(label.to_string(), action));
                if confirmed {
                    rows.push(row(
                        locale
                            .text("  Keep running", "  继续运行", "  繼續執行")
                            .to_string(),
                        TeamDockAction::KeepRunning,
                    ));
                }
            }
        }
        if !self.team.is_mutable() {
            rows.push(row(
                locale.text("Read only", "只读", "唯讀").to_string(),
                TeamDockAction::None,
            ));
        }
        if self.team.has_more() {
            rows.push(row(
                locale
                    .text(
                        "Load more team records",
                        "加载更多团队记录",
                        "載入更多團隊記錄",
                    )
                    .to_string(),
                TeamDockAction::More,
            ));
        }
        if self.team.is_limited() {
            rows.push(row(
                locale
                    .text(
                        "Showing the first 1,000 team records",
                        "显示前 1,000 条团队记录",
                        "顯示前 1,000 筆團隊記錄",
                    )
                    .to_string(),
                TeamDockAction::None,
            ));
        }
        rows.push(row(
            locale
                .text("Refresh team", "刷新团队", "重新整理團隊")
                .to_string(),
            TeamDockAction::Refresh,
        ));
        rows
    }

    pub fn activate_team_dock_row(&mut self) -> Option<Outcome> {
        let row = self.dock_rows().get(self.dock_selection?).cloned()?;
        let DockRow::Team { action, .. } = row else {
            return None;
        };
        let effects = match action {
            TeamDockAction::None => Vec::new(),
            TeamDockAction::Inspect(id) => {
                self.team_detail = if self.team_detail.as_ref() == Some(&id) {
                    None
                } else {
                    Some(id)
                };
                self.team_confirm_cancel = None;
                Vec::new()
            }
            TeamDockAction::Open(id) => {
                return Some(self.open_session_effects(id));
            }
            TeamDockAction::ConfirmStop(request) => {
                self.team_confirm_cancel = Some(request);
                Vec::new()
            }
            TeamDockAction::KeepRunning => {
                self.team_confirm_cancel = None;
                Vec::new()
            }
            TeamDockAction::Control(request) if self.team.is_mutable() && !self.team_busy => {
                self.team_busy = true;
                self.team_confirm_cancel = None;
                vec![Effect::ControlTeam { request }]
            }
            TeamDockAction::Acknowledge(request) if self.team.is_mutable() && !self.team_busy => {
                self.team_busy = true;
                vec![Effect::AcknowledgeTeam { request }]
            }
            TeamDockAction::Refresh => self.load_team(false).into_iter().collect(),
            TeamDockAction::More => self.load_team(true).into_iter().collect(),
            _ => Vec::new(),
        };
        Some(Outcome {
            effects,
            dirty: true,
        })
    }
}

#[cfg(test)]
#[path = "team_tests.rs"]
mod tests;

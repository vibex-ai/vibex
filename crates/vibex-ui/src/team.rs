//! Framework-neutral team projection and generation-fenced loading for compact
//! clients. Durable tasks, results and permissions remain on the runtime.

use std::sync::Arc;

use vibex_backend::{
    AgentBackend, BackendFuture, BackendOperation, BackendResult, DomainCapabilities,
};
use vibex_core::{
    AgentDelegationId, DelegationTaskEventKind, DelegationTaskPhase, DelegationTaskView,
    TeamSnapshot, TeamSnapshotRequest, VibexSessionId,
};

use crate::{AsyncPhase, AsyncState, locale::Locale};

const RETAINED_TEAM_ITEMS: usize = 1_000;

#[derive(Debug, Clone)]
pub struct TeamLoadTicket {
    generation: u64,
    serial: u64,
    append: bool,
    request: TeamSnapshotRequest,
}

impl TeamLoadTicket {
    pub fn request(&self) -> &TeamSnapshotRequest {
        &self.request
    }
    pub fn session_id(&self) -> &VibexSessionId {
        &self.request.session_id
    }
}

pub struct TeamWorkflowController {
    backend: Arc<dyn AgentBackend>,
    capabilities: DomainCapabilities,
    session_id: Option<VibexSessionId>,
    generation: u64,
    serial: u64,
    snapshot: AsyncState<TeamSnapshot>,
    limited: bool,
    refresh_pending: bool,
}

impl TeamWorkflowController {
    pub fn new(backend: Arc<dyn AgentBackend>, capabilities: DomainCapabilities) -> Self {
        Self {
            backend,
            capabilities,
            session_id: None,
            generation: 0,
            serial: 0,
            snapshot: AsyncState::default(),
            limited: false,
            refresh_pending: false,
        }
    }

    pub fn select_session(&mut self, session_id: VibexSessionId) {
        if self.session_id.as_ref() == Some(&session_id) {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.session_id = Some(session_id);
        self.snapshot.clear();
        self.limited = false;
        self.refresh_pending = false;
    }

    pub fn is_supported(&self) -> bool {
        self.capabilities.supports(BackendOperation::AgentTeamRead)
    }
    pub fn is_mutable(&self) -> bool {
        self.capabilities
            .supports(BackendOperation::AgentTeamMutate)
    }
    pub fn session_id(&self) -> Option<&VibexSessionId> {
        self.session_id.as_ref()
    }
    pub fn state(&self) -> &AsyncState<TeamSnapshot> {
        &self.snapshot
    }
    pub fn snapshot(&self) -> Option<&TeamSnapshot> {
        self.snapshot.value.as_ref()
    }
    pub fn is_limited(&self) -> bool {
        self.limited
    }
    pub fn has_pending_refresh(&self) -> bool {
        self.refresh_pending && !self.snapshot.is_loading()
    }
    pub fn has_more(&self) -> bool {
        !self.limited
            && self
                .snapshot()
                .is_some_and(|s| s.has_more_tasks || s.has_more_executions || s.inbox.has_more)
    }

    pub fn begin_load(&mut self, append: bool) -> Option<TeamLoadTicket> {
        if !self.is_supported() || (append && !self.has_more()) {
            return None;
        }
        if self.snapshot.is_loading() {
            self.refresh_pending |= !append;
            return None;
        }
        if !append {
            self.refresh_pending = false;
        }
        let mut request = TeamSnapshotRequest::new(self.session_id.clone()?);
        if append && let Some(snapshot) = self.snapshot() {
            // A finished collection still advances from its last item while
            // another collection has more pages. None would restart it and
            // turn an exhausted page back into a repeating first page.
            request.task_cursor = snapshot.next_task_cursor.clone().or_else(|| {
                snapshot
                    .tasks
                    .last()
                    .and_then(|task| task.task_ref.task_id())
                    .map(|id| id.to_string())
            });
            request.execution_cursor = snapshot.next_execution_cursor.clone().or_else(|| {
                snapshot
                    .executions
                    .last()
                    .map(|execution| execution.id.to_string())
            });
            request.after_event_cursor = Some(snapshot.inbox.next_cursor);
        }
        self.serial = self.serial.wrapping_add(1);
        self.snapshot.begin();
        Some(TeamLoadTicket {
            generation: self.generation,
            serial: self.serial,
            append,
            request,
        })
    }

    pub fn load(&self, ticket: &TeamLoadTicket) -> BackendFuture<'static, TeamSnapshot> {
        let backend = self.backend.clone();
        let request = ticket.request.clone();
        Box::pin(async move { backend.team_snapshot(request).await })
    }

    pub fn apply(&mut self, ticket: &TeamLoadTicket, result: BackendResult<TeamSnapshot>) -> bool {
        if self.generation != ticket.generation
            || self.serial != ticket.serial
            || self.session_id.as_ref() != Some(ticket.session_id())
        {
            return false;
        }
        match result {
            Err(error) => self.snapshot.reject(error),
            Ok(mut incoming) => {
                if ticket.append
                    && let Some(previous) = self.snapshot.value.take()
                    && previous.root_session_id == incoming.root_session_id
                {
                    for task in previous.tasks.into_iter().rev() {
                        if !incoming
                            .tasks
                            .iter()
                            .any(|next| next.task_ref == task.task_ref)
                        {
                            incoming.tasks.insert(0, task);
                        }
                    }
                    for execution in previous.executions.into_iter().rev() {
                        if !incoming
                            .executions
                            .iter()
                            .any(|next| next.id == execution.id)
                        {
                            incoming.executions.insert(0, execution);
                        }
                    }
                    for event in previous.inbox.events.into_iter().rev() {
                        if !incoming
                            .inbox
                            .events
                            .iter()
                            .any(|next| next.event_id == event.event_id)
                        {
                            incoming.inbox.events.insert(0, event);
                        }
                    }
                }
                self.limited = incoming.tasks.len() > RETAINED_TEAM_ITEMS
                    || incoming.executions.len() > RETAINED_TEAM_ITEMS
                    || incoming.inbox.events.len() > RETAINED_TEAM_ITEMS;
                incoming.tasks.truncate(RETAINED_TEAM_ITEMS);
                incoming.executions.truncate(RETAINED_TEAM_ITEMS);
                incoming.inbox.events.truncate(RETAINED_TEAM_ITEMS);
                self.snapshot.resolve(incoming);
            }
        }
        true
    }

    pub fn pending_result_events(&self, task: &DelegationTaskView) -> Vec<String> {
        self.snapshot()
            .into_iter()
            .flat_map(|s| &s.inbox.events)
            .filter(|event| {
                !event.acknowledged
                    && event.task_ref.as_ref() == Some(&task.task_ref)
                    && matches!(
                        event.kind,
                        DelegationTaskEventKind::TaskResultAvailable
                            | DelegationTaskEventKind::ExecutionFinished
                            | DelegationTaskEventKind::ExecutionAmbiguous
                    )
            })
            .take(vibex_core::TEAM_PAGE_LIMIT)
            .map(|event| event.event_id.clone())
            .collect()
    }

    pub fn task(&self, id: &AgentDelegationId) -> Option<&DelegationTaskView> {
        self.snapshot()?
            .tasks
            .iter()
            .find(|task| task.task_ref.task_id().as_ref() == Some(id))
    }

    pub fn status_label(&self, locale: Locale) -> &'static str {
        if !self.is_supported() {
            return locale.text(
                "Team views unavailable",
                "团队视图不可用",
                "團隊檢視無法使用",
            );
        }
        match self.snapshot.phase {
            AsyncPhase::Loading => locale.text("Loading team…", "正在加载团队…", "正在載入團隊…"),
            AsyncPhase::Failed => locale.text(
                "Couldn’t load team. Refresh to retry.",
                "无法加载团队，请刷新重试。",
                "無法載入團隊，請重新整理。",
            ),
            _ => locale.text("No delegated tasks", "暂无委派任务", "尚無委派任務"),
        }
    }
}

pub fn team_task_state_label(task: &DelegationTaskView, locale: Locale) -> &'static str {
    if task.blocked_on.is_some() && !task.phase.is_terminal() {
        return locale.text("Needs input", "等待输入", "等待輸入");
    }
    match task.phase {
        DelegationTaskPhase::Queued => locale.text("Queued", "已排队", "已排入佇列"),
        DelegationTaskPhase::Starting => locale.text("Starting", "正在启动", "正在啟動"),
        DelegationTaskPhase::Active => locale.text("Running", "运行中", "執行中"),
        DelegationTaskPhase::AwaitingReview => {
            locale.text("Awaiting review", "等待审阅", "等待審閱")
        }
        DelegationTaskPhase::Completed
            if task.completion_policy == vibex_core::DelegationCompletionPolicy::OwnerReview =>
        {
            locale.text("Accepted", "已验收", "已驗收")
        }
        DelegationTaskPhase::Completed => locale.text("Completed", "已完成", "已完成"),
        DelegationTaskPhase::Failed => locale.text("Failed", "失败", "失敗"),
        DelegationTaskPhase::Cancelled => locale.text("Cancelled", "已取消", "已取消"),
        DelegationTaskPhase::Cancelling => locale.text("Stopping", "正在停止", "正在停止"),
    }
}

pub fn team_execution_outcome_label(
    outcome: vibex_core::ExecutionOutcome,
    locale: Locale,
) -> &'static str {
    use vibex_core::ExecutionOutcome::*;
    match outcome {
        Completed => locale.text("Completed", "已完成", "已完成"),
        ActionsOnly => locale.text("Actions only", "仅执行操作", "僅執行操作"),
        EmptyReply => locale.text("Empty reply", "空回复", "空回覆"),
        Refusal => locale.text("Declined", "已拒绝", "已拒絕"),
        MaxTokens => locale.text("Token limit", "达到 token 上限", "達到 token 上限"),
        AuthRequired => locale.text("Sign-in required", "需要登录", "需要登入"),
        Cancelled => locale.text("Cancelled", "已取消", "已取消"),
        Failed => locale.text("Failed", "失败", "失敗"),
        Ambiguous => locale.text("Outcome unknown", "结果未知", "結果未知"),
        Running => locale.text("Running", "运行中", "執行中"),
        Queued => locale.text("Queued", "已排队", "已排入佇列"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_backend::{BackendCapabilitySnapshot, DisconnectedBackend};

    fn task(
        session: &VibexSessionId,
        name: &str,
        phase: DelegationTaskPhase,
    ) -> DelegationTaskView {
        serde_json::from_value(serde_json::json!({
            "taskRef": vibex_core::VibexUseRef::task(&AgentDelegationId::parse(format!("delegation_{name}")).unwrap()),
            "sessionRef": vibex_core::VibexUseRef::session(&VibexSessionId::new()),
            "parentSessionRef": vibex_core::VibexUseRef::session(session),
            "rootSessionRef": vibex_core::VibexUseRef::session(session),
            "title": name, "phase": phase, "legacyStatus": "running",
            "ownershipKind": "owned_child", "completionPolicy": "owner_review",
            "revision": 1, "createdAtMs": 1, "updatedAtMs": 1
        })).unwrap()
    }

    fn snapshot(session: &VibexSessionId, tasks: Vec<DelegationTaskView>) -> TeamSnapshot {
        serde_json::from_value(serde_json::json!({
            "rootSessionId": session, "tasks": tasks, "executions": [],
            "inbox": {"events": [], "nextCursor": 0, "hasMore": false},
            "tree": {"nodes": [], "registry": [], "nextCursor": null, "hasMore": false}, "groups": [],
            "capability": { "delivery": "mcp", "authority": "test",
                "callerSessionRef": vibex_core::VibexUseRef::session(session),
                "rootSessionRef": vibex_core::VibexUseRef::session(session),
                "currentWorkspaceRef": vibex_core::VibexUseRef::workspace("workspace_test"),
                "catalogRevision": 1, "activationRevision": 1, "remainingExecutions": 4,
                "maxDepth": 2, "remainingDepth": 2, "canDelegate": true, "canReadAnySession": false,
                "presentation": vibex_core::VibexUsePresentationCapability::default(), "availableTools": [] },
            "nextTaskCursor": null, "hasMoreTasks": false, "nextExecutionCursor": null, "hasMoreExecutions": false
        })).unwrap()
    }

    fn event(
        task: &DelegationTaskView,
        id: &str,
        cursor: i64,
        acknowledged: bool,
    ) -> vibex_core::DelegationTaskEvent {
        vibex_core::DelegationTaskEvent {
            cursor,
            event_id: id.to_string(),
            kind: DelegationTaskEventKind::TaskResultAvailable,
            task_ref: Some(task.task_ref.clone()),
            session_ref: task.session_ref.clone(),
            root_session_ref: Some(task.root_session_ref.clone()),
            revision: 1,
            payload: serde_json::json!({}),
            occurred_at_ms: cursor,
            delivered: false,
            acknowledged,
        }
    }

    fn controller(session: &VibexSessionId) -> TeamWorkflowController {
        let mut controller = TeamWorkflowController::new(
            Arc::new(DisconnectedBackend),
            BackendCapabilitySnapshot::desktop_native_v1().agent,
        );
        controller.select_session(session.clone());
        controller
    }

    #[test]
    fn inbox_pagination_does_not_restart_exhausted_task_pages_or_duplicate_events() {
        let session = VibexSessionId::new();
        let mut controller = controller(&session);
        let task = task(&session, "one", DelegationTaskPhase::AwaitingReview);
        let mut first = snapshot(&session, vec![task.clone()]);
        first.inbox.events = vec![event(&task, "one", 1, false)];
        first.inbox.has_more = true;
        first.inbox.next_cursor = 1;
        let ticket = controller.begin_load(false).unwrap();
        assert!(controller.apply(&ticket, Ok(first)));
        let more = controller.begin_load(true).unwrap();
        assert_eq!(
            more.request.task_cursor,
            task.task_ref.task_id().map(|id| id.to_string())
        );
        assert_eq!(more.request.after_event_cursor, Some(1));
        let mut second = snapshot(&session, Vec::new());
        second.inbox.events = vec![event(&task, "one", 1, false), event(&task, "two", 2, false)];
        second.inbox.next_cursor = 2;
        assert!(controller.apply(&more, Ok(second)));
        let loaded = controller.snapshot().unwrap();
        assert_eq!(loaded.tasks.len(), 1);
        assert_eq!(loaded.inbox.events.len(), 2);
        assert!(!controller.has_more());
    }

    #[test]
    fn human_result_collection_is_separate_from_acceptance() {
        let session = VibexSessionId::new();
        let mut controller = controller(&session);
        let task = task(&session, "result", DelegationTaskPhase::AwaitingReview);
        let other = self::task(&session, "other", DelegationTaskPhase::AwaitingReview);
        let mut loaded = snapshot(&session, vec![task.clone()]);
        loaded.inbox.events = vec![
            event(&task, "read", 1, true),
            event(&task, "pending", 2, false),
            event(&other, "unrelated", 3, false),
        ];
        let ticket = controller.begin_load(false).unwrap();
        controller.apply(&ticket, Ok(loaded));
        assert_eq!(controller.pending_result_events(&task), vec!["pending"]);
        assert_eq!(team_task_state_label(&task, Locale::En), "Awaiting review");
        let mut accepted = task.clone();
        accepted.phase = DelegationTaskPhase::Completed;
        assert_eq!(team_task_state_label(&accepted, Locale::En), "Accepted");
        accepted.completion_policy = vibex_core::DelegationCompletionPolicy::SingleTurnLegacy;
        assert_eq!(team_task_state_label(&accepted, Locale::En), "Completed");
        let labels = [
            DelegationTaskPhase::Active,
            DelegationTaskPhase::AwaitingReview,
            DelegationTaskPhase::Completed,
            DelegationTaskPhase::Failed,
            DelegationTaskPhase::Cancelled,
            DelegationTaskPhase::Cancelling,
        ]
        .map(|phase| {
            let mut value = task.clone();
            value.phase = phase;
            team_task_state_label(&value, Locale::En)
        });
        assert_eq!(
            labels
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            6
        );
    }

    #[test]
    fn team_projection_keeps_a_bounded_number_of_records() {
        let session = VibexSessionId::new();
        let mut controller = controller(&session);
        let tasks = (0..=RETAINED_TEAM_ITEMS)
            .map(|index| task(&session, &index.to_string(), DelegationTaskPhase::Active))
            .collect();
        let mut loaded = snapshot(&session, tasks);
        loaded.has_more_tasks = true;
        let ticket = controller.begin_load(false).unwrap();
        controller.apply(&ticket, Ok(loaded));
        assert_eq!(
            controller.snapshot().unwrap().tasks.len(),
            RETAINED_TEAM_ITEMS
        );
        assert!(controller.is_limited());
        assert!(!controller.has_more());
    }

    #[test]
    fn a_read_only_team_can_load_but_cannot_mutate() {
        let session = VibexSessionId::new();
        let capabilities = DomainCapabilities::available([BackendOperation::AgentTeamRead]);
        let mut controller =
            TeamWorkflowController::new(Arc::new(DisconnectedBackend), capabilities);
        controller.select_session(session);
        assert!(controller.begin_load(false).is_some());
        assert!(!controller.is_mutable());
    }

    #[test]
    fn team_events_during_a_load_schedule_one_followup_refresh() {
        let session = VibexSessionId::new();
        let mut controller = controller(&session);
        let first = controller.begin_load(false).unwrap();
        assert!(controller.begin_load(false).is_none());
        assert!(controller.begin_load(false).is_none());
        assert!(!controller.has_pending_refresh());
        assert!(controller.apply(&first, Ok(snapshot(&session, Vec::new()))));
        assert!(controller.has_pending_refresh());
        let followup = controller.begin_load(false).unwrap();
        assert!(!controller.has_pending_refresh());
        assert!(!controller.apply(&first, Ok(snapshot(&session, Vec::new()))));
        assert!(controller.apply(&followup, Ok(snapshot(&session, Vec::new()))));
        assert!(!controller.has_pending_refresh());
    }

    #[test]
    fn team_load_rejects_a_result_after_switching_sessions() {
        let backend = Arc::new(DisconnectedBackend);
        let mut controller = TeamWorkflowController::new(
            backend,
            BackendCapabilitySnapshot::desktop_native_v1().agent,
        );
        controller.select_session(VibexSessionId::new());
        let old = controller.begin_load(false).unwrap();
        controller.select_session(VibexSessionId::new());
        assert!(!controller.apply(
            &old,
            Err(vibex_backend::BackendError::offline("offline", "offline"))
        ));
        assert_eq!(controller.state().phase, AsyncPhase::Idle);
        assert!(controller.state().error.is_none());
    }
}
